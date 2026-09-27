//! Host-neutral asset/variant edit sessions with bounded, generation-aware
//! preview and export scheduling (Lap `docs/raw-development/spec.md`,
//! "Recipe and session contract").
//!
//! The [`SessionManager`] replaces the RapidRAW singleton `AppState` editor
//! state with explicit per-session resources. It knows nothing about windows,
//! Tauri, catalogs, or the filesystem: hosts inject the actual pixel work via
//! [`PreviewRenderer`] / [`ExportRenderer`] and durable recipe persistence via
//! [`RecipeStore`], which keeps the scheduling policy deterministic and
//! testable without a GPU.
//!
//! Contracts (spec, "Proposed host operations"):
//!
//! - `open` — a session owns its original buffer ([`Arc<DecodedOriginal>`]),
//!   its committed recipe envelope, and its own preview generation counter.
//!   Concurrent sessions never share mutable state (spec A2/A4 isolation).
//! - `render_preview` — requests carry an explicit, strictly increasing
//!   generation; at most one queued and one in-flight preview exists per
//!   session, and accepting a newer generation coalesces (cancels) the older
//!   queued request and cancels the in-flight one. Obsolete completions are
//!   dropped: a delayed old preview can never overwrite a newer
//!   session/generation result (spec A4). Results carry session, asset,
//!   variant, and generation identifiers.
//! - `commit` — validates the envelope identity and recipe, serializes saves
//!   per session, applies optimistic revision checks (one of two same-revision
//!   commits succeeds, the other reports a conflict), and counts as a durable
//!   save. Preview cancellation cannot cancel a save.
//! - `export` — a separate bounded queue over immutable snapshots: the
//!   committed envelope and original buffer are captured at enqueue time;
//!   later edits, preview cancellation, or session closure never alter or
//!   cancel durable export work.
//! - `close` — cancels the session's previews, waits for pending saves to
//!   settle, then releases the session's buffers and cached previews.
//!   Session caches and queues are bounded by configuration; diagnostics
//!   counters expose the ceilings needed for memory/performance
//!   qualification (spec A12).

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rapidraw_edit_model::{RecipeEnvelope, validate_recipe};

use crate::decode::{CancelSource, CancelToken, DecodedOriginal};
use crate::error::DevelopError;

/// Opaque edit-session identifier, unique per [`SessionManager`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SessionId(pub u64);

/// Opaque durable-export job identifier, unique per [`SessionManager`].
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ExportJobId(pub u64);

/// Bounded-capacity configuration. Every queue and cache is bounded by an
/// explicit field; zero worker counts and zero bounds are rejected at
/// construction (the preview-result cache may be disabled with `0`).
#[derive(Clone, Copy, Debug)]
pub struct SessionManagerConfig {
    /// Maximum concurrently open sessions; idle sessions are evicted
    /// least-recently-used first. Bounds CPU/GPU memory under navigation.
    pub max_sessions: usize,
    /// Preview worker threads (bounded in-flight previews).
    pub preview_workers: usize,
    /// Export worker threads (bounded in-flight exports; a queue separate
    /// from previews).
    pub export_workers: usize,
    /// Maximum pending (queued) preview jobs across all sessions.
    pub max_queued_previews: usize,
    /// Maximum pending (queued) export jobs across all sessions.
    pub max_queued_exports: usize,
    /// Maximum cached preview frames per session (bounded result cache).
    pub max_cached_preview_results: usize,
}

impl Default for SessionManagerConfig {
    fn default() -> Self {
        Self {
            max_sessions: 8,
            preview_workers: 1,
            export_workers: 1,
            max_queued_previews: 16,
            max_queued_exports: 4,
            max_cached_preview_results: 2,
        }
    }
}

/// Preview quality tier requested by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PreviewQuality {
    /// Fast draft while a control is being dragged.
    Interactive,
    /// Full-quality settled preview after interaction stops.
    Settled,
}

/// Errors surfaced by the session manager. Every failure is typed and
/// explicit; nothing is silently dropped except explicitly cancelled or
/// superseded preview work (reported as outcomes, not errors).
#[derive(Debug)]
pub enum SessionError {
    /// No session with this id exists (never opened, or closed).
    NotFound { session_id: SessionId },
    /// The session exists but is closing/closed; it accepts no new work.
    SessionClosed { session_id: SessionId },
    /// The requested preview generation is not strictly newer than the
    /// session's most recently accepted generation.
    StaleGeneration {
        session_id: SessionId,
        accepted: u64,
        requested: u64,
    },
    /// Optimistic concurrency failure: another commit already advanced the
    /// revision (spec A4: one of two same-revision commits succeeds).
    RevisionConflict {
        session_id: SessionId,
        current: u64,
        expected: u64,
    },
    /// Open would exceed [`SessionManagerConfig::max_sessions`] and no
    /// session is idle enough to evict.
    TooManySessions { limit: usize },
    /// The bounded preview queue is full.
    PreviewQueueFull { limit: usize },
    /// The bounded export queue is full.
    ExportQueueFull { limit: usize },
    /// Envelope identity mismatch, invalid recipe, or invalid configuration.
    InvalidInput(String),
    /// The injected preview/export renderer failed.
    Render(DevelopError),
    /// The injected recipe store rejected the durable save. The session
    /// keeps its dirty state for retry.
    Persist(String),
    /// The job was cancelled through its own cancellation domain.
    Cancelled,
    /// The manager is shutting down.
    ManagerShuttingDown,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::NotFound { session_id } => write!(f, "session {session_id:?} not found"),
            SessionError::SessionClosed { session_id } => {
                write!(f, "session {session_id:?} is closed")
            }
            SessionError::StaleGeneration {
                session_id,
                accepted,
                requested,
            } => write!(
                f,
                "preview generation {requested} is stale for session {session_id:?}; accepted generation is {accepted}"
            ),
            SessionError::RevisionConflict {
                session_id,
                current,
                expected,
            } => write!(
                f,
                "revision conflict on session {session_id:?}: expected {expected}, current is {current}"
            ),
            SessionError::TooManySessions { limit } => {
                write!(
                    f,
                    "session limit of {limit} reached and no idle session to evict"
                )
            }
            SessionError::PreviewQueueFull { limit } => {
                write!(f, "preview queue limit of {limit} reached")
            }
            SessionError::ExportQueueFull { limit } => {
                write!(f, "export queue limit of {limit} reached")
            }
            SessionError::InvalidInput(message) => write!(f, "invalid session input: {message}"),
            SessionError::Render(error) => write!(f, "render failed: {error}"),
            SessionError::Persist(message) => write!(f, "durable save failed: {message}"),
            SessionError::Cancelled => write!(f, "job cancelled"),
            SessionError::ManagerShuttingDown => write!(f, "session manager is shutting down"),
        }
    }
}

impl std::error::Error for SessionError {}

/// Host-injected preview pixel work (GPU offscreen render in the real host;
/// deterministic fakes in tests). Implementations should honor
/// [`PreviewJob::cancel`] cooperatively: cancellation is domain-separated and
/// affects only the preview job carrying the token.
pub trait PreviewRenderer: Send + Sync + 'static {
    fn render(&self, job: &PreviewJob) -> Result<PreviewFrame, DevelopError>;
}

/// Host-injected durable export pixel work. Export jobs are rendered from
/// immutable committed snapshots; they are never cancelled by preview
/// scheduling or session closure.
pub trait ExportRenderer: Send + Sync + 'static {
    fn render(&self, job: &ExportJob) -> Result<ExportFrame, DevelopError>;
}

/// Host-injected durable recipe persistence (atomic sidecar write in the
/// real host). Called outside all manager locks, serialized per session.
pub trait RecipeStore: Send + Sync + 'static {
    fn persist(&self, envelope: &RecipeEnvelope) -> Result<(), String>;
}

/// One preview render job handed to the [`PreviewRenderer`]. The original
/// buffer and envelope are immutable snapshots owned by the job.
pub struct PreviewJob {
    pub session_id: SessionId,
    pub asset_id: String,
    pub variant_id: String,
    pub generation: u64,
    pub quality: PreviewQuality,
    /// Maximum preview edge length requested by the host.
    pub max_edge: u32,
    pub original: Arc<DecodedOriginal>,
    /// Immutable recipe snapshot for this request (may be dirtier than the
    /// committed envelope; the host renders what the user sees).
    pub envelope: RecipeEnvelope,
    /// Preview-domain cancellation token.
    pub cancel: CancelToken,
}

/// Rendered preview pixels (RGBA8, sRGB-encoded by host convention).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// One durable export render job. Everything is captured at enqueue time;
/// later commits do not change it (immutable input).
pub struct ExportJob {
    pub job_id: ExportJobId,
    pub asset_id: String,
    pub variant_id: String,
    /// Committed revision captured at enqueue (`envelope.revision`).
    pub revision: u64,
    pub original: Arc<DecodedOriginal>,
    pub envelope: RecipeEnvelope,
    /// Optional output edge limit; `None` renders at original dimensions.
    pub max_edge: Option<u32>,
    /// Export-domain cancellation token; independent of previews.
    pub cancel: CancelToken,
}

/// Rendered export pixels (RGBA8, sRGB-encoded by host convention).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExportFrame {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// Identity carried by every preview result so hosts can drop stale
/// responses by session and generation (spec A4).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewTicketInfo {
    pub session_id: SessionId,
    pub asset_id: String,
    pub variant_id: String,
    pub generation: u64,
    pub quality: PreviewQuality,
    pub max_edge: u32,
}

/// Final state of one preview request. Obsolete work is reported as
/// [`PreviewOutcome::Cancelled`], never as a success for a stale generation.
#[derive(Debug)]
pub enum PreviewOutcome {
    Completed {
        info: PreviewTicketInfo,
        frame: PreviewFrame,
        render_duration: Duration,
    },
    Cancelled {
        info: PreviewTicketInfo,
    },
    Failed {
        info: PreviewTicketInfo,
        error: SessionError,
    },
}

/// Handle for awaiting one preview request's outcome.
pub struct PreviewTicket {
    pub info: PreviewTicketInfo,
    pub outcome: Receiver<PreviewOutcome>,
}

/// Identity carried by every export result.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportJobInfo {
    pub job_id: ExportJobId,
    pub asset_id: String,
    pub variant_id: String,
    pub revision: u64,
}

/// Final state of one export job.
#[derive(Debug)]
pub enum ExportOutcome {
    Completed {
        info: ExportJobInfo,
        frame: ExportFrame,
        render_duration: Duration,
    },
    Cancelled {
        info: ExportJobInfo,
    },
    Failed {
        info: ExportJobInfo,
        error: SessionError,
    },
}

/// Handle for awaiting one export job's outcome.
pub struct ExportTicket {
    pub job: ExportJobInfo,
    pub outcome: Receiver<ExportOutcome>,
}

/// Request to open a session. The host has already decoded the original and
/// loaded the committed envelope; the manager takes ownership of the session
/// resources.
pub struct OpenSessionRequest {
    pub asset_id: String,
    pub variant_id: String,
    /// SHA-256 (or host-equivalent fingerprint) of the untouched source bytes.
    pub source_fingerprint: String,
    pub original: Arc<DecodedOriginal>,
    /// Current committed envelope for this asset/variant.
    pub envelope: RecipeEnvelope,
}

/// Result of a successful `open`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OpenedSession {
    pub session_id: SessionId,
    pub asset_id: String,
    pub variant_id: String,
    pub revision: u64,
    pub dimensions: (u32, u32),
    pub source_fingerprint: String,
}

/// One validated, durably acknowledged recipe commit.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CommitResult {
    pub session_id: SessionId,
    pub revision: u64,
}

/// Request to render a preview for a session at an explicit generation.
pub struct PreviewRequest {
    pub session_id: SessionId,
    /// Strictly increasing per session; older generations are rejected, and
    /// accepting a new generation coalesces/cancels older preview work.
    pub generation: u64,
    pub recipe: RecipeEnvelope,
    pub quality: PreviewQuality,
    pub max_edge: u32,
}

/// Cache lookup key for preview results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewCacheKey {
    pub generation: u64,
    pub quality: PreviewQuality,
    pub max_edge: u32,
}

/// Live session state snapshot.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub asset_id: String,
    pub variant_id: String,
    pub revision: u64,
    pub accepted_generation: u64,
}

/// Result of a successful `close`: the session's resources are released.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClosedSession {
    pub session_id: SessionId,
    pub final_revision: u64,
    pub released_preview_cache_entries: usize,
    pub released_preview_cache_bytes: usize,
}

/// Counters and queue depths for memory/performance qualification
/// (spec A12). All values are instantaneous snapshots.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionDiagnostics {
    pub open_sessions: usize,
    pub queued_previews: usize,
    pub in_flight_previews: usize,
    pub queued_exports: usize,
    pub in_flight_exports: usize,
    /// Total bytes of cached preview frames across all sessions.
    pub cached_preview_bytes: usize,
    pub cached_preview_entries: usize,
    /// Total bytes of session-owned original buffers (CPU side).
    pub tracked_original_bytes: usize,
    pub sessions_opened: u64,
    pub sessions_closed: u64,
    pub session_evictions: u64,
    pub previews_executed: u64,
    /// Previews cancelled while queued or via cooperative token.
    pub previews_cancelled: u64,
    /// Previews whose renderer completed after a newer generation was
    /// accepted; their pixels were discarded, never delivered as success.
    pub previews_dropped_stale: u64,
    pub previews_failed: u64,
    pub exports_executed: u64,
    pub exports_cancelled: u64,
    pub exports_failed: u64,
    pub commits_ok: u64,
    pub commits_failed: u64,
}

struct PreviewEntry {
    session: Arc<SessionShared>,
    info: PreviewTicketInfo,
    recipe: RecipeEnvelope,
    original: Arc<DecodedOriginal>,
    cancel_source: CancelSource,
    cancel_token: CancelToken,
    ticket: Sender<PreviewOutcome>,
    /// Set once an outcome has been delivered (or the entry superseded);
    /// guards exactly-once outcome delivery.
    notified: AtomicBool,
    cancelled: AtomicBool,
}

struct ExportEntry {
    info: ExportJobInfo,
    job: ExportJob,
    cancel_source: CancelSource,
    ticket: Sender<ExportOutcome>,
    notified: AtomicBool,
    cancelled: AtomicBool,
}

struct SessionState {
    open: bool,
    envelope: RecipeEnvelope,
    accepted_generation: u64,
    queued: Option<Arc<PreviewEntry>>,
    in_flight: Option<Arc<PreviewEntry>>,
    in_flight_generation: Option<u64>,
    pending_saves: u32,
    preview_cache: VecDeque<(PreviewCacheKey, PreviewFrame)>,
    cached_entries: usize,
    cached_bytes: usize,
    last_access: u64,
}

struct SessionShared {
    id: SessionId,
    asset_id: String,
    variant_id: String,
    source_fingerprint: String,
    original: Arc<DecodedOriginal>,
    original_bytes: usize,
    /// Serializes durable commits per session (two windows, one save).
    commit_lock: Mutex<()>,
    /// Signalled whenever `pending_saves` returns to zero; `close` waits on
    /// it so closure never abandons a durable save.
    saves_cv: Condvar,
    state: Mutex<SessionState>,
}

struct ManagerState {
    /// Ordered by session id (creation order) for deterministic scans.
    sessions: BTreeMap<SessionId, Arc<SessionShared>>,
    preview_queue: VecDeque<Arc<PreviewEntry>>,
    export_queue: VecDeque<Arc<ExportEntry>>,
    export_in_flight: Option<Arc<ExportEntry>>,
    next_session_ordinal: u64,
    next_export_job_ordinal: u64,
}

struct Counters {
    in_flight_previews: AtomicUsize,
    in_flight_exports: AtomicUsize,
    cached_preview_bytes: AtomicUsize,
    cached_preview_entries: AtomicUsize,
    tracked_original_bytes: AtomicUsize,
    sessions_opened: AtomicU64,
    sessions_closed: AtomicU64,
    session_evictions: AtomicU64,
    previews_executed: AtomicU64,
    previews_cancelled: AtomicU64,
    previews_dropped_stale: AtomicU64,
    previews_failed: AtomicU64,
    exports_executed: AtomicU64,
    exports_cancelled: AtomicU64,
    exports_failed: AtomicU64,
    commits_ok: AtomicU64,
    commits_failed: AtomicU64,
}

struct ManagerShared {
    config: SessionManagerConfig,
    preview_renderer: Arc<dyn PreviewRenderer>,
    export_renderer: Arc<dyn ExportRenderer>,
    store: Arc<dyn RecipeStore>,
    state: Mutex<ManagerState>,
    preview_cv: Condvar,
    export_cv: Condvar,
    shutdown: AtomicBool,
    access_epoch: AtomicU64,
    counters: Counters,
}

fn touch(shared: &ManagerShared, state: &mut SessionState) {
    state.last_access = shared.access_epoch.fetch_add(1, Ordering::SeqCst);
}

fn finalize_preview(entry: &PreviewEntry, outcome: PreviewOutcome) {
    if !entry.notified.swap(true, Ordering::SeqCst) {
        let _ = entry.ticket.send(outcome);
    }
}

fn finalize_export(entry: &ExportEntry, outcome: ExportOutcome) {
    if !entry.notified.swap(true, Ordering::SeqCst) {
        let _ = entry.ticket.send(outcome);
    }
}

/// Host-neutral edit-session manager. See the [module docs](self) for the
/// open/render/commit/export/close contracts.
pub struct SessionManager {
    shared: Arc<ManagerShared>,
    preview_handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
    export_handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl SessionManager {
    /// Validates the configuration, constructs the manager, and starts the
    /// bounded preview and export worker pools.
    pub fn new(
        config: SessionManagerConfig,
        preview_renderer: Arc<dyn PreviewRenderer>,
        export_renderer: Arc<dyn ExportRenderer>,
        store: Arc<dyn RecipeStore>,
    ) -> Result<Self, SessionError> {
        if config.max_sessions == 0
            || config.preview_workers == 0
            || config.export_workers == 0
            || config.max_queued_previews == 0
            || config.max_queued_exports == 0
        {
            return Err(SessionError::InvalidInput(
                "session manager bounds and worker counts must be non-zero".to_string(),
            ));
        }

        let shared = Arc::new(ManagerShared {
            config,
            preview_renderer,
            export_renderer,
            store,
            state: Mutex::new(ManagerState {
                sessions: BTreeMap::new(),
                preview_queue: VecDeque::new(),
                export_queue: VecDeque::new(),
                export_in_flight: None,
                next_session_ordinal: 0,
                next_export_job_ordinal: 0,
            }),
            preview_cv: Condvar::new(),
            export_cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
            access_epoch: AtomicU64::new(0),
            counters: Counters {
                in_flight_previews: AtomicUsize::new(0),
                in_flight_exports: AtomicUsize::new(0),
                cached_preview_bytes: AtomicUsize::new(0),
                cached_preview_entries: AtomicUsize::new(0),
                tracked_original_bytes: AtomicUsize::new(0),
                sessions_opened: AtomicU64::new(0),
                sessions_closed: AtomicU64::new(0),
                session_evictions: AtomicU64::new(0),
                previews_executed: AtomicU64::new(0),
                previews_cancelled: AtomicU64::new(0),
                previews_dropped_stale: AtomicU64::new(0),
                previews_failed: AtomicU64::new(0),
                exports_executed: AtomicU64::new(0),
                exports_cancelled: AtomicU64::new(0),
                exports_failed: AtomicU64::new(0),
                commits_ok: AtomicU64::new(0),
                commits_failed: AtomicU64::new(0),
            },
        });

        let preview_handles = (0..config.preview_workers)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("rapidraw-session-preview".to_string())
                    .spawn(move || preview_worker(shared))
                    .expect("spawn preview worker")
            })
            .collect();
        let export_handles = (0..config.export_workers)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("rapidraw-session-export".to_string())
                    .spawn(move || export_worker(shared))
                    .expect("spawn export worker")
            })
            .collect();

        Ok(Self {
            shared,
            preview_handles: Mutex::new(preview_handles),
            export_handles: Mutex::new(export_handles),
        })
    }

    /// Opens a session, taking ownership of the supplied original buffer and
    /// committed envelope. Evicts least-recently-used idle sessions when the
    /// session cache is full.
    pub fn open_session(&self, request: OpenSessionRequest) -> Result<OpenedSession, SessionError> {
        if request.asset_id.is_empty() || request.variant_id.is_empty() {
            return Err(SessionError::InvalidInput(
                "asset id and variant id must be non-empty".to_string(),
            ));
        }
        if request.envelope.asset_id != request.asset_id
            || request.envelope.variant_id != request.variant_id
        {
            return Err(SessionError::InvalidInput(format!(
                "envelope identity ({}, {}) does not match the session request ({}, {})",
                request.envelope.asset_id,
                request.envelope.variant_id,
                request.asset_id,
                request.variant_id
            )));
        }
        if request.envelope.revision == 0 {
            return Err(SessionError::InvalidInput(
                "committed envelope revision must be >= 1".to_string(),
            ));
        }

        let shared = &self.shared;
        let mut state = shared.state.lock().unwrap();
        if shared.shutdown.load(Ordering::SeqCst) {
            return Err(SessionError::ManagerShuttingDown);
        }

        // Evict idle sessions while at capacity. Victims must have no queued
        // or in-flight preview and no pending durable save.
        while state.sessions.len() >= shared.config.max_sessions {
            let victim = state
                .sessions
                .values()
                .filter(|session| {
                    let sst = session.state.lock().unwrap();
                    sst.open
                        && sst.queued.is_none()
                        && sst.in_flight.is_none()
                        && sst.pending_saves == 0
                })
                .min_by_key(|session| session.state.lock().unwrap().last_access)
                .cloned();
            let victim = match victim {
                Some(victim) => victim,
                None => {
                    return Err(SessionError::TooManySessions {
                        limit: shared.config.max_sessions,
                    });
                }
            };
            let mut sst = victim.state.lock().unwrap();
            sst.open = false;
            if let Some(entry) = sst.queued.take() {
                entry.cancelled.store(true, Ordering::SeqCst);
                entry.cancel_source.cancel();
                finalize_preview(
                    &entry,
                    PreviewOutcome::Cancelled {
                        info: entry.info.clone(),
                    },
                );
                shared
                    .counters
                    .previews_cancelled
                    .fetch_add(1, Ordering::SeqCst);
            }
            if let Some(entry) = sst.in_flight.take() {
                entry.cancelled.store(true, Ordering::SeqCst);
                entry.cancel_source.cancel();
            }
            let cached_bytes = sst.cached_bytes;
            let cached_entries = sst.cached_entries;
            drop(sst);
            state.sessions.remove(&victim.id);
            drop(state);
            release_closed_counters(shared, cached_entries, cached_bytes, victim.original_bytes);
            shared
                .counters
                .sessions_closed
                .fetch_add(1, Ordering::SeqCst);
            shared
                .counters
                .session_evictions
                .fetch_add(1, Ordering::SeqCst);
            drop(victim);
            state = shared.state.lock().unwrap();
        }

        state.next_session_ordinal += 1;
        let id = SessionId(state.next_session_ordinal);
        let (width, height) = request.original.image.dimensions();
        let original_bytes = width as usize * height as usize * 3 * std::mem::size_of::<f32>();
        let session = Arc::new(SessionShared {
            id,
            asset_id: request.asset_id.clone(),
            variant_id: request.variant_id.clone(),
            source_fingerprint: request.source_fingerprint.clone(),
            original: Arc::clone(&request.original),
            original_bytes,
            commit_lock: Mutex::new(()),
            saves_cv: Condvar::new(),
            state: Mutex::new(SessionState {
                open: true,
                envelope: request.envelope,
                accepted_generation: 0,
                queued: None,
                in_flight: None,
                in_flight_generation: None,
                pending_saves: 0,
                preview_cache: VecDeque::new(),
                cached_entries: 0,
                cached_bytes: 0,
                last_access: 0,
            }),
        });
        {
            let mut sst = session.state.lock().unwrap();
            touch(shared, &mut sst);
        }
        state.sessions.insert(id, Arc::clone(&session));
        shared
            .counters
            .tracked_original_bytes
            .fetch_add(original_bytes, Ordering::SeqCst);
        shared
            .counters
            .sessions_opened
            .fetch_add(1, Ordering::SeqCst);
        drop(state);

        Ok(OpenedSession {
            session_id: id,
            asset_id: session.asset_id.clone(),
            variant_id: session.variant_id.clone(),
            revision: session.state.lock().unwrap().envelope.revision,
            dimensions: (width, height),
            source_fingerprint: session.source_fingerprint.clone(),
        })
    }

    /// Accepts a preview request. See the module docs for the
    /// generation/coalescing policy.
    pub fn render_preview(&self, request: PreviewRequest) -> Result<PreviewTicket, SessionError> {
        if request.generation == 0 {
            return Err(SessionError::InvalidInput(
                "preview generation must be >= 1".to_string(),
            ));
        }
        let shared = &self.shared;
        let mut state = shared.state.lock().unwrap();
        if shared.shutdown.load(Ordering::SeqCst) {
            return Err(SessionError::ManagerShuttingDown);
        }
        let session =
            state
                .sessions
                .get(&request.session_id)
                .cloned()
                .ok_or(SessionError::NotFound {
                    session_id: request.session_id,
                })?;
        let mut sst = session.state.lock().unwrap();
        if !sst.open {
            return Err(SessionError::SessionClosed {
                session_id: request.session_id,
            });
        }
        if request.generation <= sst.accepted_generation {
            return Err(SessionError::StaleGeneration {
                session_id: request.session_id,
                accepted: sst.accepted_generation,
                requested: request.generation,
            });
        }
        // Bounded queue. Replacing this session's own queued entry is
        // allowed even at capacity (it is dropped below, freeing a slot).
        let pending = state
            .preview_queue
            .iter()
            .filter(|entry| !entry.notified.load(Ordering::SeqCst))
            .count();
        if pending >= shared.config.max_queued_previews && sst.queued.is_none() {
            return Err(SessionError::PreviewQueueFull {
                limit: shared.config.max_queued_previews,
            });
        }
        sst.accepted_generation = request.generation;
        touch(shared, &mut sst);

        // Coalesce: drop this session's queued request, cancel in-flight.
        if let Some(entry) = sst.queued.take() {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
            finalize_preview(
                &entry,
                PreviewOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
            shared
                .counters
                .previews_cancelled
                .fetch_add(1, Ordering::SeqCst);
        }
        if let Some(entry) = &sst.in_flight {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
        }

        let info = PreviewTicketInfo {
            session_id: session.id,
            asset_id: session.asset_id.clone(),
            variant_id: session.variant_id.clone(),
            generation: request.generation,
            quality: request.quality,
            max_edge: request.max_edge,
        };
        let (cancel_source, cancel_token) = CancelToken::pair();
        let (tx, rx) = channel();
        let entry = Arc::new(PreviewEntry {
            session: Arc::clone(&session),
            info: info.clone(),
            recipe: request.recipe,
            original: Arc::clone(&session.original),
            cancel_source,
            cancel_token,
            ticket: tx,
            notified: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        sst.queued = Some(Arc::clone(&entry));
        state.preview_queue.push_back(entry);
        drop(sst);
        drop(state);
        shared.preview_cv.notify_one();

        Ok(PreviewTicket { info, outcome: rx })
    }

    /// Cancels a single queued or in-flight preview generation. Returns true
    /// when a job was found. Never touches exports or saves.
    pub fn cancel_preview(&self, session_id: SessionId, generation: u64) -> bool {
        let shared = &self.shared;
        let state = shared.state.lock().unwrap();
        let Some(session) = state.sessions.get(&session_id) else {
            return false;
        };
        let mut sst = session.state.lock().unwrap();
        if let Some(entry) = &sst.queued
            && entry.info.generation == generation
        {
            let entry = sst.queued.take().unwrap();
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
            finalize_preview(
                &entry,
                PreviewOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
            shared
                .counters
                .previews_cancelled
                .fetch_add(1, Ordering::SeqCst);
            return true;
        }
        if let Some(entry) = &sst.in_flight
            && sst.in_flight_generation == Some(generation)
        {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
            return true;
        }
        false
    }

    /// Cancels all preview work of a session (queued dropped, in-flight
    /// tokens fired). Returns the number of affected jobs. Never touches
    /// exports or saves.
    pub fn cancel_session_previews(&self, session_id: SessionId) -> usize {
        let shared = &self.shared;
        let state = shared.state.lock().unwrap();
        let Some(session) = state.sessions.get(&session_id) else {
            return 0;
        };
        let mut sst = session.state.lock().unwrap();
        let mut cancelled = 0;
        if let Some(entry) = sst.queued.take() {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
            finalize_preview(
                &entry,
                PreviewOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
            shared
                .counters
                .previews_cancelled
                .fetch_add(1, Ordering::SeqCst);
            cancelled += 1;
        }
        if let Some(entry) = sst.in_flight.take() {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
            cancelled += 1;
        }
        cancelled
    }

    /// Validates and durably persists a recipe. Commits are serialized per
    /// session; `expected_revision` must match the session's current
    /// revision or a [`SessionError::RevisionConflict`] is returned. A failed
    /// persist keeps the session's previous committed envelope for retry.
    pub fn commit_recipe(
        &self,
        session_id: SessionId,
        expected_revision: u64,
        envelope: RecipeEnvelope,
    ) -> Result<CommitResult, SessionError> {
        let shared = &self.shared;
        let session = {
            let state = shared.state.lock().unwrap();
            state
                .sessions
                .get(&session_id)
                .cloned()
                .ok_or(SessionError::NotFound { session_id })?
        };
        // Reject closing/closed sessions before the commit lock: an
        // in-flight save holding the lock must never block the rejection.
        {
            let sst = session.state.lock().unwrap();
            if !sst.open {
                return Err(SessionError::SessionClosed { session_id });
            }
        }
        // Serialize durable saves per session: the second of two concurrent
        // commits observes the first's revision and reports a conflict.
        let _commit_guard = session.commit_lock.lock().unwrap();
        let mut sst = session.state.lock().unwrap();
        if !sst.open {
            return Err(SessionError::SessionClosed { session_id });
        }
        if envelope.asset_id != session.asset_id || envelope.variant_id != session.variant_id {
            return Err(SessionError::InvalidInput(format!(
                "envelope identity ({}, {}) does not match the session asset ({}, {})",
                envelope.asset_id, envelope.variant_id, session.asset_id, session.variant_id
            )));
        }
        validate_recipe(&envelope.recipe)
            .map_err(|error| SessionError::InvalidInput(error.to_string()))?;
        if sst.envelope.revision != expected_revision {
            return Err(SessionError::RevisionConflict {
                session_id,
                current: sst.envelope.revision,
                expected: expected_revision,
            });
        }

        let mut durable = envelope;
        durable.revision = expected_revision + 1;
        sst.pending_saves = 1;
        touch(shared, &mut sst);
        drop(sst);

        // The store is called with no manager or session locks held: a slow
        // save never blocks rendering, and preview cancellation (a different
        // domain) cannot reach it.
        let result = shared.store.persist(&durable);

        let mut sst = session.state.lock().unwrap();
        sst.pending_saves = 0;
        drop(sst);
        session.saves_cv.notify_all();

        match result {
            Ok(()) => {
                let mut sst = session.state.lock().unwrap();
                sst.envelope = durable;
                drop(sst);
                shared.counters.commits_ok.fetch_add(1, Ordering::SeqCst);
                Ok(CommitResult {
                    session_id,
                    revision: expected_revision + 1,
                })
            }
            Err(message) => {
                // The committed envelope is untouched; the session keeps its
                // dirty state for retry.
                shared
                    .counters
                    .commits_failed
                    .fetch_add(1, Ordering::SeqCst);
                Err(SessionError::Persist(message))
            }
        }
    }

    /// Enqueues a durable export of the session's last committed envelope
    /// and original buffer, captured immutably at enqueue time.
    pub fn export_from_session(
        &self,
        session_id: SessionId,
        max_edge: Option<u32>,
    ) -> Result<ExportTicket, SessionError> {
        let shared = &self.shared;
        let mut state = shared.state.lock().unwrap();
        if shared.shutdown.load(Ordering::SeqCst) {
            return Err(SessionError::ManagerShuttingDown);
        }
        let session = state
            .sessions
            .get(&session_id)
            .cloned()
            .ok_or(SessionError::NotFound { session_id })?;
        {
            let sst = session.state.lock().unwrap();
            if !sst.open {
                return Err(SessionError::SessionClosed { session_id });
            }
        }
        let envelope = session.state.lock().unwrap().envelope.clone();
        let revision = envelope.revision;
        self.enqueue_export(
            &mut state,
            shared,
            session.asset_id.clone(),
            session.variant_id.clone(),
            revision,
            envelope,
            Arc::clone(&session.original),
            max_edge,
        )
    }

    /// Enqueues a durable export from an explicit immutable snapshot (for
    /// assets without an open session).
    pub fn export_snapshot(
        &self,
        asset_id: String,
        variant_id: String,
        envelope: RecipeEnvelope,
        original: Arc<DecodedOriginal>,
        max_edge: Option<u32>,
    ) -> Result<ExportTicket, SessionError> {
        let shared = &self.shared;
        let mut state = shared.state.lock().unwrap();
        if shared.shutdown.load(Ordering::SeqCst) {
            return Err(SessionError::ManagerShuttingDown);
        }
        let revision = envelope.revision;
        self.enqueue_export(
            &mut state, shared, asset_id, variant_id, revision, envelope, original, max_edge,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn enqueue_export(
        &self,
        state: &mut ManagerState,
        shared: &Arc<ManagerShared>,
        asset_id: String,
        variant_id: String,
        revision: u64,
        envelope: RecipeEnvelope,
        original: Arc<DecodedOriginal>,
        max_edge: Option<u32>,
    ) -> Result<ExportTicket, SessionError> {
        let pending = state
            .export_queue
            .iter()
            .filter(|entry| !entry.notified.load(Ordering::SeqCst))
            .count();
        if pending >= shared.config.max_queued_exports {
            return Err(SessionError::ExportQueueFull {
                limit: shared.config.max_queued_exports,
            });
        }
        state.next_export_job_ordinal += 1;
        let job_id = ExportJobId(state.next_export_job_ordinal);
        let info = ExportJobInfo {
            job_id,
            asset_id: asset_id.clone(),
            variant_id: variant_id.clone(),
            revision,
        };
        let (cancel_source, cancel_token) = CancelToken::pair();
        let (tx, rx) = channel();
        let entry = Arc::new(ExportEntry {
            info: info.clone(),
            job: ExportJob {
                job_id,
                asset_id,
                variant_id,
                revision,
                original,
                envelope,
                max_edge,
                cancel: cancel_token,
            },
            cancel_source,
            ticket: tx,
            notified: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        state.export_queue.push_back(entry);
        // The caller's manager guard is still held here; notifying under the
        // lock is safe (workers re-check the queue before waiting).
        shared.export_cv.notify_one();
        Ok(ExportTicket {
            job: info,
            outcome: rx,
        })
    }

    /// Cancels one export job (queued: dropped; in-flight: cooperative
    /// token). Returns true when the job was found.
    pub fn cancel_export(&self, job_id: ExportJobId) -> bool {
        let shared = &self.shared;
        let mut state = shared.state.lock().unwrap();
        if let Some(index) = state
            .export_queue
            .iter()
            .position(|entry| entry.info.job_id == job_id && !entry.notified.load(Ordering::SeqCst))
        {
            let entry = state.export_queue.remove(index).unwrap();
            entry.cancelled.store(true, Ordering::SeqCst);
            finalize_export(
                &entry,
                ExportOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
            shared
                .counters
                .exports_cancelled
                .fetch_add(1, Ordering::SeqCst);
            return true;
        }
        if let Some(entry) = &state.export_in_flight
            && entry.info.job_id == job_id
        {
            entry.cancelled.store(true, Ordering::SeqCst);
            entry.cancel_source.cancel();
            return true;
        }
        false
    }

    /// Closes a session: cancels its previews, waits for pending saves to
    /// settle, then releases its buffers and cached previews. In-flight
    /// durable exports are not cancelled.
    pub fn close_session(&self, session_id: SessionId) -> Result<ClosedSession, SessionError> {
        let shared = &self.shared;
        let session = {
            let state = shared.state.lock().unwrap();
            state
                .sessions
                .get(&session_id)
                .cloned()
                .ok_or(SessionError::NotFound { session_id })?
        };

        // Mark the session closing so it rejects new work, and cancel its
        // previews (the preview cancellation domain only). The manager lock
        // is held (unnamed guard) so eviction cannot race this close.
        {
            let _manager_lock = shared.state.lock().unwrap();
            let mut sst = session.state.lock().unwrap();
            if !sst.open {
                return Err(SessionError::SessionClosed { session_id });
            }
            sst.open = false;
            if let Some(entry) = sst.queued.take() {
                entry.cancelled.store(true, Ordering::SeqCst);
                entry.cancel_source.cancel();
                finalize_preview(
                    &entry,
                    PreviewOutcome::Cancelled {
                        info: entry.info.clone(),
                    },
                );
                shared
                    .counters
                    .previews_cancelled
                    .fetch_add(1, Ordering::SeqCst);
            }
            if let Some(entry) = sst.in_flight.take() {
                entry.cancelled.store(true, Ordering::SeqCst);
                entry.cancel_source.cancel();
            }
        }

        // Durable saves must settle before resources are released. The save
        // path notifies this condvar when `pending_saves` returns to zero.
        {
            let mut sst = session.state.lock().unwrap();
            while sst.pending_saves > 0 {
                sst = session.saves_cv.wait(sst).unwrap();
            }
        }

        let mut state = shared.state.lock().unwrap();
        state.sessions.remove(&session_id);
        let (cached_entries, cached_bytes, final_revision) = {
            let sst = session.state.lock().unwrap();
            (sst.cached_entries, sst.cached_bytes, sst.envelope.revision)
        };
        drop(state);
        release_closed_counters(shared, cached_entries, cached_bytes, session.original_bytes);
        shared
            .counters
            .sessions_closed
            .fetch_add(1, Ordering::SeqCst);
        drop(session);

        Ok(ClosedSession {
            session_id,
            final_revision,
            released_preview_cache_entries: cached_entries,
            released_preview_cache_bytes: cached_bytes,
        })
    }

    /// Snapshot of a session's identity/revision state (also refreshes its
    /// LRU recency). Returns `None` for unknown or closing/closed sessions.
    pub fn session_info(&self, session_id: SessionId) -> Option<SessionInfo> {
        let shared = &self.shared;
        let state = shared.state.lock().unwrap();
        let session = state.sessions.get(&session_id)?;
        let mut sst = session.state.lock().unwrap();
        if !sst.open {
            return None;
        }
        touch(shared, &mut sst);
        Some(SessionInfo {
            session_id,
            asset_id: session.asset_id.clone(),
            variant_id: session.variant_id.clone(),
            revision: sst.envelope.revision,
            accepted_generation: sst.accepted_generation,
        })
    }

    /// Clone of the session's last committed envelope.
    pub fn session_envelope(&self, session_id: SessionId) -> Option<RecipeEnvelope> {
        let shared = &self.shared;
        let state = shared.state.lock().unwrap();
        let session = state.sessions.get(&session_id)?;
        let mut sst = session.state.lock().unwrap();
        if !sst.open {
            return None;
        }
        touch(shared, &mut sst);
        Some(sst.envelope.clone())
    }

    /// Cached preview result, if still resident.
    pub fn cached_preview(
        &self,
        session_id: SessionId,
        key: PreviewCacheKey,
    ) -> Option<PreviewFrame> {
        let shared = &self.shared;
        let state = shared.state.lock().unwrap();
        let session = state.sessions.get(&session_id)?;
        let mut sst = session.state.lock().unwrap();
        if !sst.open {
            return None;
        }
        let index = sst
            .preview_cache
            .iter()
            .position(|(cached, _)| *cached == key)?;
        let (cached, frame) = sst.preview_cache.remove(index)?;
        sst.preview_cache.push_back((cached, frame.clone()));
        Some(frame)
    }

    /// Instantaneous counters and queue depths.
    pub fn diagnostics(&self) -> SessionDiagnostics {
        let shared = &self.shared;
        let counters = &shared.counters;
        let state = shared.state.lock().unwrap();
        let mut open_sessions = 0;
        for session in state.sessions.values() {
            if session.state.lock().unwrap().open {
                open_sessions += 1;
            }
        }
        SessionDiagnostics {
            open_sessions,
            queued_previews: state
                .preview_queue
                .iter()
                .filter(|entry| !entry.notified.load(Ordering::SeqCst))
                .count(),
            in_flight_previews: counters.in_flight_previews.load(Ordering::SeqCst),
            queued_exports: state
                .export_queue
                .iter()
                .filter(|entry| !entry.notified.load(Ordering::SeqCst))
                .count(),
            in_flight_exports: counters.in_flight_exports.load(Ordering::SeqCst),
            cached_preview_bytes: counters.cached_preview_bytes.load(Ordering::SeqCst),
            cached_preview_entries: counters.cached_preview_entries.load(Ordering::SeqCst),
            tracked_original_bytes: counters.tracked_original_bytes.load(Ordering::SeqCst),
            sessions_opened: counters.sessions_opened.load(Ordering::SeqCst),
            sessions_closed: counters.sessions_closed.load(Ordering::SeqCst),
            session_evictions: counters.session_evictions.load(Ordering::SeqCst),
            previews_executed: counters.previews_executed.load(Ordering::SeqCst),
            previews_cancelled: counters.previews_cancelled.load(Ordering::SeqCst),
            previews_dropped_stale: counters.previews_dropped_stale.load(Ordering::SeqCst),
            previews_failed: counters.previews_failed.load(Ordering::SeqCst),
            exports_executed: counters.exports_executed.load(Ordering::SeqCst),
            exports_cancelled: counters.exports_cancelled.load(Ordering::SeqCst),
            exports_failed: counters.exports_failed.load(Ordering::SeqCst),
            commits_ok: counters.commits_ok.load(Ordering::SeqCst),
            commits_failed: counters.commits_failed.load(Ordering::SeqCst),
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.preview_cv.notify_all();
        self.shared.export_cv.notify_all();
        let preview_handles = std::mem::take(&mut *self.preview_handles.lock().unwrap());
        for handle in preview_handles {
            let _ = handle.join();
        }
        let export_handles = std::mem::take(&mut *self.export_handles.lock().unwrap());
        for handle in export_handles {
            let _ = handle.join();
        }
        // Workers are gone: deliver cancellation to anything still pending
        // so ticket holders never hang on a dropped manager.
        let mut state = self.shared.state.lock().unwrap();
        for entry in state.preview_queue.drain(..) {
            finalize_preview(
                &entry,
                PreviewOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
        }
        if let Some(entry) = state.export_in_flight.take() {
            finalize_export(
                &entry,
                ExportOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
        }
        for entry in state.export_queue.drain(..) {
            finalize_export(
                &entry,
                ExportOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
        }
    }
}

fn release_closed_counters(
    shared: &ManagerShared,
    cached_entries: usize,
    cached_bytes: usize,
    original_bytes: usize,
) {
    let counters = &shared.counters;
    counters
        .cached_preview_entries
        .fetch_sub(cached_entries, Ordering::SeqCst);
    counters
        .cached_preview_bytes
        .fetch_sub(cached_bytes, Ordering::SeqCst);
    counters
        .tracked_original_bytes
        .fetch_sub(original_bytes, Ordering::SeqCst);
}

fn insert_cache(
    config: &SessionManagerConfig,
    state: &mut SessionState,
    counters: &Counters,
    key: PreviewCacheKey,
    frame: PreviewFrame,
) {
    if config.max_cached_preview_results == 0 {
        return;
    }
    let bytes = frame.rgba8.len();
    state.preview_cache.push_back((key, frame));
    state.cached_entries += 1;
    state.cached_bytes += bytes;
    counters
        .cached_preview_entries
        .fetch_add(1, Ordering::SeqCst);
    counters
        .cached_preview_bytes
        .fetch_add(bytes, Ordering::SeqCst);
    while state.preview_cache.len() > config.max_cached_preview_results {
        if let Some((_, evicted)) = state.preview_cache.pop_front() {
            state.cached_entries -= 1;
            state.cached_bytes -= evicted.rgba8.len();
            counters
                .cached_preview_entries
                .fetch_sub(1, Ordering::SeqCst);
            counters
                .cached_preview_bytes
                .fetch_sub(evicted.rgba8.len(), Ordering::SeqCst);
        }
    }
}

fn preview_worker(shared: Arc<ManagerShared>) {
    loop {
        let entry = {
            let mut state = shared.state.lock().unwrap();
            loop {
                // Drop already-finalized entries, finalize entries of closed
                // sessions, and dispatch the first runnable job (FIFO).
                let mut dispatch = None;
                let index = 0;
                while index < state.preview_queue.len() {
                    let entry = Arc::clone(&state.preview_queue[index]);
                    if entry.notified.load(Ordering::SeqCst) {
                        state.preview_queue.remove(index);
                        continue;
                    }
                    let open = entry.session.state.lock().unwrap().open;
                    if !open {
                        state.preview_queue.remove(index);
                        finalize_preview(
                            &entry,
                            PreviewOutcome::Cancelled {
                                info: entry.info.clone(),
                            },
                        );
                        shared
                            .counters
                            .previews_cancelled
                            .fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    // Remove from the queue before dispatch: an in-flight
                    // job is no longer pending, and must never be picked up
                    // twice.
                    state.preview_queue.remove(index);
                    {
                        let mut sst = entry.session.state.lock().unwrap();
                        sst.queued = None;
                        sst.in_flight = Some(Arc::clone(&entry));
                        sst.in_flight_generation = Some(entry.info.generation);
                    }
                    dispatch = Some(entry);
                    break;
                }
                if let Some(entry) = dispatch {
                    break entry;
                }
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                state = shared.preview_cv.wait(state).unwrap();
            }
        };
        run_preview_entry(&shared, entry);
    }
}

fn run_preview_entry(shared: &ManagerShared, entry: Arc<PreviewEntry>) {
    let started = Instant::now();
    let job = PreviewJob {
        session_id: entry.info.session_id,
        asset_id: entry.info.asset_id.clone(),
        variant_id: entry.info.variant_id.clone(),
        generation: entry.info.generation,
        quality: entry.info.quality,
        max_edge: entry.info.max_edge,
        original: Arc::clone(&entry.original),
        envelope: entry.recipe.clone(),
        cancel: entry.cancel_token.clone(),
    };
    shared
        .counters
        .in_flight_previews
        .fetch_add(1, Ordering::SeqCst);
    let result = shared.preview_renderer.render(&job);
    drop(job);
    shared
        .counters
        .in_flight_previews
        .fetch_sub(1, Ordering::SeqCst);
    let elapsed = started.elapsed();

    let mut sst = entry.session.state.lock().unwrap();
    sst.in_flight = None;
    sst.in_flight_generation = None;
    let stale = !sst.open
        || entry.cancelled.load(Ordering::SeqCst)
        || entry.info.generation != sst.accepted_generation;
    if stale {
        match result {
            Ok(_) => {
                shared
                    .counters
                    .previews_executed
                    .fetch_add(1, Ordering::SeqCst);
                shared
                    .counters
                    .previews_dropped_stale
                    .fetch_add(1, Ordering::SeqCst);
            }
            Err(DevelopError::Cancelled) => {
                shared
                    .counters
                    .previews_cancelled
                    .fetch_add(1, Ordering::SeqCst);
            }
            Err(_) => {
                shared
                    .counters
                    .previews_failed
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
        drop(sst);
        finalize_preview(
            &entry,
            PreviewOutcome::Cancelled {
                info: entry.info.clone(),
            },
        );
        return;
    }
    match result {
        Ok(frame) => {
            shared
                .counters
                .previews_executed
                .fetch_add(1, Ordering::SeqCst);
            let key = PreviewCacheKey {
                generation: entry.info.generation,
                quality: entry.info.quality,
                max_edge: entry.info.max_edge,
            };
            insert_cache(
                &shared.config,
                &mut sst,
                &shared.counters,
                key,
                frame.clone(),
            );
            drop(sst);
            finalize_preview(
                &entry,
                PreviewOutcome::Completed {
                    info: entry.info.clone(),
                    frame,
                    render_duration: elapsed,
                },
            );
        }
        Err(DevelopError::Cancelled) => {
            drop(sst);
            shared
                .counters
                .previews_cancelled
                .fetch_add(1, Ordering::SeqCst);
            finalize_preview(
                &entry,
                PreviewOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
        }
        Err(error) => {
            drop(sst);
            shared
                .counters
                .previews_failed
                .fetch_add(1, Ordering::SeqCst);
            finalize_preview(
                &entry,
                PreviewOutcome::Failed {
                    info: entry.info.clone(),
                    error: SessionError::Render(error),
                },
            );
        }
    }
}

fn export_worker(shared: Arc<ManagerShared>) {
    loop {
        let entry = {
            let mut state = shared.state.lock().unwrap();
            loop {
                let mut dispatch = None;
                let index = 0;
                while index < state.export_queue.len() {
                    let entry = Arc::clone(&state.export_queue[index]);
                    if entry.notified.load(Ordering::SeqCst)
                        || entry.cancelled.load(Ordering::SeqCst)
                    {
                        state.export_queue.remove(index);
                        continue;
                    }
                    // Remove from the queue before dispatch: the job is no
                    // longer pending and must never run twice.
                    state.export_queue.remove(index);
                    state.export_in_flight = Some(Arc::clone(&entry));
                    dispatch = Some(entry);
                    break;
                }
                if let Some(entry) = dispatch {
                    break entry;
                }
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                state = shared.export_cv.wait(state).unwrap();
            }
        };
        run_export_entry(&shared, entry);
    }
}

fn run_export_entry(shared: &ManagerShared, entry: Arc<ExportEntry>) {
    let started = Instant::now();
    shared
        .counters
        .in_flight_exports
        .fetch_add(1, Ordering::SeqCst);
    let result = shared.export_renderer.render(&entry.job);
    shared
        .counters
        .in_flight_exports
        .fetch_sub(1, Ordering::SeqCst);
    let elapsed = started.elapsed();

    {
        let mut state = shared.state.lock().unwrap();
        if state
            .export_in_flight
            .as_ref()
            .is_some_and(|in_flight| Arc::ptr_eq(in_flight, &entry))
        {
            state.export_in_flight = None;
        }
    }

    // Durable work is never dropped as stale; only its own cancellation
    // domain (or a renderer failure) can change the outcome.
    match result {
        Ok(frame) => {
            shared
                .counters
                .exports_executed
                .fetch_add(1, Ordering::SeqCst);
            finalize_export(
                &entry,
                ExportOutcome::Completed {
                    info: entry.info.clone(),
                    frame,
                    render_duration: elapsed,
                },
            );
        }
        Err(DevelopError::Cancelled) => {
            shared
                .counters
                .exports_cancelled
                .fetch_add(1, Ordering::SeqCst);
            finalize_export(
                &entry,
                ExportOutcome::Cancelled {
                    info: entry.info.clone(),
                },
            );
        }
        Err(error) => {
            shared
                .counters
                .exports_failed
                .fetch_add(1, Ordering::SeqCst);
            finalize_export(
                &entry,
                ExportOutcome::Failed {
                    info: entry.info.clone(),
                    error: SessionError::Render(error),
                },
            );
        }
    }
}
