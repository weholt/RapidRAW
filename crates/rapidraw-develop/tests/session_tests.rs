//! Deterministic concurrency tests for the host-neutral session subsystem
//! (`rapidraw_develop::session`), guarding Lap `docs/raw-development/spec.md`
//! A4 (stale session/generation results never overwrite newer ones), A7
//! (explicit failures, durable work never cancelled by preview scheduling)
//! and A12 (bounded memory under repeated navigation).
//!
//! Determinism comes from injected gated renderers/stores: the tests control
//! completion order via channels instead of sleeping on real render timing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rapidraw_develop::session::{
    ExportJob, ExportOutcome, ExportRenderer, OpenSessionRequest, PreviewFrame, PreviewJob,
    PreviewOutcome, PreviewQuality, PreviewRenderer, PreviewRequest, RecipeStore, SessionError,
    SessionManager, SessionManagerConfig,
};
use rapidraw_develop::{DecodeReport, DecodedOriginal, DevelopError, LinearImage, LinearRawMode};
use rapidraw_edit_model::RecipeEnvelope;
use rawler::decoders::Orientation;

const TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Fixtures and gated fakes
// ---------------------------------------------------------------------------

fn original(width: u32, height: u32, seed: f32) -> Arc<DecodedOriginal> {
    let image = LinearImage::from_fn(width, height, |x, y| {
        [seed + x as f32, seed + y as f32, seed]
    });
    Arc::new(DecodedOriginal {
        image,
        report: DecodeReport {
            source_dimensions: (width, height),
            output_dimensions: (width, height),
            orientation: Orientation::Normal,
            is_linear_raw: false,
            wb_neutralized: false,
            fast_demosaic: false,
            highlight_compression: 2.5,
            linear_mode: LinearRawMode::Auto,
            tone_mapper: None,
        },
    })
}

fn envelope(asset: &str, variant: &str, fingerprint: &str) -> RecipeEnvelope {
    let mut env = RecipeEnvelope::new(
        rapidraw_edit_model::MODEL_VERSION,
        asset,
        variant,
        fingerprint,
    );
    env.revision = 1;
    env
}

fn open_request(asset: &str, variant: &str, original: Arc<DecodedOriginal>) -> OpenSessionRequest {
    OpenSessionRequest {
        asset_id: asset.to_string(),
        variant_id: variant.to_string(),
        source_fingerprint: format!("fp-{asset}"),
        original,
        envelope: envelope(asset, variant, &format!("fp-{asset}")),
    }
}

fn config(
    max_sessions: usize,
    preview_workers: usize,
    max_queued: usize,
    cached: usize,
) -> SessionManagerConfig {
    SessionManagerConfig {
        max_sessions,
        preview_workers,
        export_workers: 1,
        max_queued_previews: max_queued,
        max_queued_exports: 1,
        max_cached_preview_results: cached,
    }
}

fn frame_of(generation: u64) -> PreviewFrame {
    PreviewFrame {
        width: 1,
        height: 1,
        rgba8: vec![
            (generation & 0xff) as u8,
            ((generation >> 8) & 0xff) as u8,
            0,
            255,
        ],
    }
}

#[derive(Default)]
struct GateRegistry {
    inner: Mutex<HashMap<(u64, u64), Receiver<()>>>,
}

impl GateRegistry {
    fn install(&self, key: (u64, u64)) -> Sender<()> {
        let (tx, rx) = mpsc::channel::<()>();
        self.inner.lock().unwrap().insert(key, rx);
        tx
    }
}

/// Preview renderer that signals started jobs and blocks on per-job gates,
/// giving the test full control over completion order. When
/// `honor_cancel_after_gate` is set it re-checks the cancellation token after
/// the gate (a cooperative renderer); otherwise it completes regardless, and
/// the manager must drop the stale result itself.
struct GatedPreviewRenderer {
    started: Sender<(u64, u64)>,
    gates: Arc<GateRegistry>,
    honor_cancel_after_gate: bool,
}

impl PreviewRenderer for GatedPreviewRenderer {
    fn render(&self, job: &PreviewJob) -> Result<PreviewFrame, DevelopError> {
        if job.cancel.is_cancelled() {
            return Err(DevelopError::Cancelled);
        }
        let _ = self.started.send((job.session_id.0, job.generation));
        let gate = self
            .gates
            .inner
            .lock()
            .unwrap()
            .remove(&(job.session_id.0, job.generation));
        if let Some(gate) = gate {
            let _ = gate.recv();
            if self.honor_cancel_after_gate && job.cancel.is_cancelled() {
                return Err(DevelopError::Cancelled);
            }
        }
        Ok(frame_of(job.generation))
    }
}

/// Export renderer with per-job gates; embeds the snapshot revision and
/// original size into the frame so tests can assert immutable input.
struct GatedExportRenderer {
    started: Sender<(u64, u64)>,
    gates: Arc<GateRegistry>,
}

impl ExportRenderer for GatedExportRenderer {
    fn render(
        &self,
        job: &ExportJob,
    ) -> Result<rapidraw_develop::session::ExportFrame, DevelopError> {
        if job.cancel.is_cancelled() {
            return Err(DevelopError::Cancelled);
        }
        let _ = self.started.send((job.job_id.0, job.revision));
        let gate = self
            .gates
            .inner
            .lock()
            .unwrap()
            .remove(&(job.job_id.0, job.revision));
        if let Some(gate) = gate {
            let _ = gate.recv();
        }
        Ok(rapidraw_develop::session::ExportFrame {
            width: job.original.image.width(),
            height: job.original.image.height(),
            rgba8: vec![
                (job.revision & 0xff) as u8,
                ((job.revision >> 8) & 0xff) as u8,
                0,
                255,
            ],
        })
    }
}

/// Recipe store whose first `gate` persist call blocks until released; the
/// fail flag simulates write failures.
struct GatedStore {
    started: Sender<u64>,
    gate: Mutex<Option<Receiver<()>>>,
    fail: AtomicBool,
    persists: AtomicU64,
    last_revision: AtomicU64,
}

impl RecipeStore for GatedStore {
    fn persist(&self, envelope: &RecipeEnvelope) -> Result<(), String> {
        self.persists.fetch_add(1, Ordering::SeqCst);
        self.last_revision
            .store(envelope.revision, Ordering::SeqCst);
        let _ = self.started.send(envelope.revision);
        if let Some(gate) = self.gate.lock().unwrap().take() {
            let _ = gate.recv();
        }
        if self.fail.load(Ordering::SeqCst) {
            Err("simulated disk full".to_string())
        } else {
            Ok(())
        }
    }
}

fn instant_store() -> Arc<GatedStore> {
    let (tx, _rx) = mpsc::channel();
    Arc::new(GatedStore {
        started: tx,
        gate: Mutex::new(None),
        fail: AtomicBool::new(false),
        persists: AtomicU64::new(0),
        last_revision: AtomicU64::new(0),
    })
}

/// Store whose persist blocks on an explicit gate; the test keeps the
/// started-signal receiver.
fn gated_store() -> (Arc<GatedStore>, Receiver<u64>, Sender<()>) {
    let (started_tx, started_rx) = mpsc::channel::<u64>();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    (
        Arc::new(GatedStore {
            started: started_tx,
            gate: Mutex::new(Some(gate_rx)),
            fail: AtomicBool::new(false),
            persists: AtomicU64::new(0),
            last_revision: AtomicU64::new(0),
        }),
        started_rx,
        gate_tx,
    )
}

/// Blocks until the store reports persisting `revision`.
fn wait_store_started(rx: &Receiver<u64>, revision: u64) {
    loop {
        let got = rx
            .recv_timeout(TIMEOUT)
            .expect("store should have started persisting");
        if got == revision {
            return;
        }
    }
}

type StartedRx = Receiver<(u64, u64)>;

fn gated_manager(
    cfg: SessionManagerConfig,
    gates: &Arc<GateRegistry>,
    store: Arc<GatedStore>,
) -> (SessionManager, StartedRx, StartedRx) {
    let (p_tx, p_rx) = mpsc::channel();
    let (e_tx, e_rx) = mpsc::channel();
    let manager = SessionManager::new(
        cfg,
        Arc::new(GatedPreviewRenderer {
            started: p_tx,
            gates: Arc::clone(gates),
            honor_cancel_after_gate: false,
        }),
        Arc::new(GatedExportRenderer {
            started: e_tx,
            gates: Arc::clone(gates),
        }),
        store,
    )
    .expect("manager construction must succeed");
    (manager, p_rx, e_rx)
}

fn wait_started(rx: &Receiver<(u64, u64)>, expected: (u64, u64)) {
    loop {
        let got = rx
            .recv_timeout(TIMEOUT)
            .expect("renderer should have started the expected job");
        if got == expected {
            return;
        }
    }
}

fn await_preview(ticket: rapidraw_develop::session::PreviewTicket) -> PreviewOutcome {
    ticket
        .outcome
        .recv_timeout(TIMEOUT)
        .expect("preview outcome should arrive")
}

fn await_export(ticket: rapidraw_develop::session::ExportTicket) -> ExportOutcome {
    ticket
        .outcome
        .recv_timeout(TIMEOUT)
        .expect("export outcome should arrive")
}

fn completed_info(outcome: PreviewOutcome) -> rapidraw_develop::session::PreviewTicketInfo {
    match outcome {
        PreviewOutcome::Completed { info, .. } => info,
        other => panic!("expected Completed preview outcome, got {other:?}"),
    }
}

/// Unwraps a typed error without requiring `Debug` on the success type.
fn expect_err<T>(result: Result<T, SessionError>, context: &str) -> SessionError {
    match result {
        Ok(_) => panic!("expected an error ({context}) but the call succeeded"),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Two concurrent sessions keep original buffers, recipes and generation
/// counters isolated (acceptance criterion 1).
#[test]
fn concurrent_sessions_isolate_originals_recipes_and_generation_counters() {
    let gates = Arc::new(GateRegistry::default());
    let (manager, _p, _e) = gated_manager(config(8, 1, 16, 2), &gates, instant_store());

    let orig_a = original(2, 2, 10.0);
    let orig_b = original(3, 2, 20.0);
    let snapshot_a = orig_a.image.pixel(0, 0);
    let opened_a = manager
        .open_session(open_request("asset-a", "primary", Arc::clone(&orig_a)))
        .unwrap();
    let opened_b = manager
        .open_session(open_request("asset-b", "primary", Arc::clone(&orig_b)))
        .unwrap();
    assert_ne!(opened_a.session_id, opened_b.session_id);

    let ticket_a = manager
        .render_preview(PreviewRequest {
            session_id: opened_a.session_id,
            generation: 1,
            recipe: envelope("asset-a", "primary", "fp-asset-a"),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        })
        .unwrap();
    let ticket_b = manager
        .render_preview(PreviewRequest {
            session_id: opened_b.session_id,
            generation: 77,
            recipe: envelope("asset-b", "primary", "fp-asset-b"),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        })
        .unwrap();

    let info_a = completed_info(await_preview(ticket_a));
    let info_b = completed_info(await_preview(ticket_b));
    assert_eq!(info_a.session_id, opened_a.session_id);
    assert_eq!(info_a.asset_id, "asset-a");
    assert_eq!(info_a.generation, 1);
    assert_eq!(info_b.session_id, opened_b.session_id);
    assert_eq!(info_b.asset_id, "asset-b");
    assert_eq!(info_b.generation, 77);

    // Generation counters are isolated per session.
    assert_eq!(
        manager
            .session_info(opened_a.session_id)
            .unwrap()
            .accepted_generation,
        1
    );
    assert_eq!(
        manager
            .session_info(opened_b.session_id)
            .unwrap()
            .accepted_generation,
        77
    );

    // Original buffers are immutable through rendering.
    assert_eq!(orig_a.image.pixel(0, 0), snapshot_a);

    // A recipe change on A never appears in B's committed envelope.
    let mut env_a2 = envelope("asset-a", "primary", "fp-asset-a");
    env_a2.revision = 2;
    env_a2.recipe.exposure = 0.5;
    manager
        .commit_recipe(opened_a.session_id, 1, env_a2)
        .unwrap();
    assert_eq!(
        manager.session_info(opened_b.session_id).unwrap().revision,
        1
    );
    assert_eq!(
        manager
            .session_envelope(opened_b.session_id)
            .unwrap()
            .recipe
            .exposure,
        0.0
    );
    assert_eq!(
        manager
            .session_envelope(opened_a.session_id)
            .unwrap()
            .recipe
            .exposure,
        0.5
    );
}

/// Previews coalesce: a delayed old-generation completion is dropped and the
/// newer generation wins; results carry session/generation identifiers and
/// the per-session result cache stays bounded (acceptance criterion 2).
#[test]
fn previews_coalesce_and_stale_completions_never_overwrite_newer_generation() {
    let gates = Arc::new(GateRegistry::default());
    let (manager, p_rx, _e) = gated_manager(config(8, 1, 16, 2), &gates, instant_store());
    let opened = manager
        .open_session(open_request("asset-a", "primary", original(2, 2, 1.0)))
        .unwrap();

    let gate1 = gates.install((opened.session_id.0, 1));
    let gate2 = gates.install((opened.session_id.0, 2));

    let request = |generation: u64| PreviewRequest {
        session_id: opened.session_id,
        generation,
        recipe: envelope("asset-a", "primary", "fp-asset-a"),
        quality: PreviewQuality::Settled,
        max_edge: 512,
    };

    let ticket1 = manager.render_preview(request(1)).unwrap();
    wait_started(&p_rx, (opened.session_id.0, 1));

    // Accepting generation 2 supersedes in-flight generation 1.
    let ticket2 = manager.render_preview(request(2)).unwrap();
    let info2 = ticket2.info.clone();
    assert_eq!(info2.generation, 2);

    let _ = gate1.send(());
    match await_preview(ticket1) {
        PreviewOutcome::Cancelled { info } => {
            assert_eq!(info.generation, 1);
            assert_eq!(info.session_id, opened.session_id);
        }
        other => panic!("delayed old preview must be cancelled, got {other:?}"),
    }

    let _ = gate2.send(());
    let info = completed_info(await_preview(ticket2));
    assert_eq!(info.generation, 2);

    // Cached result belongs to the current generation only.
    let key2 = rapidraw_develop::session::PreviewCacheKey {
        generation: 2,
        quality: PreviewQuality::Settled,
        max_edge: 512,
    };
    assert!(manager.cached_preview(opened.session_id, key2).is_some());
    assert!(
        manager
            .cached_preview(
                opened.session_id,
                rapidraw_develop::session::PreviewCacheKey {
                    generation: 1,
                    quality: PreviewQuality::Settled,
                    max_edge: 512,
                }
            )
            .is_none()
    );

    // Bounded cache: a third settled result evicts the oldest.
    let ticket3 = manager.render_preview(request(3)).unwrap();
    assert!(matches!(
        await_preview(ticket3),
        PreviewOutcome::Completed { .. }
    ));
    let ticket4 = manager.render_preview(request(4)).unwrap();
    assert!(matches!(
        await_preview(ticket4),
        PreviewOutcome::Completed { .. }
    ));
    let diag = manager.diagnostics();
    assert_eq!(
        diag.cached_preview_entries, 2,
        "cache bound is max_cached_preview_results"
    );
    assert!(
        manager
            .cached_preview(
                opened.session_id,
                rapidraw_develop::session::PreviewCacheKey {
                    generation: 2,
                    quality: PreviewQuality::Settled,
                    max_edge: 512,
                }
            )
            .is_none()
    );
    assert_eq!(diag.previews_dropped_stale, 1);

    // Older generations are rejected after a newer one was accepted.
    match expect_err(
        manager.render_preview(request(3)),
        "stale generation rejection",
    ) {
        SessionError::StaleGeneration {
            accepted,
            requested,
            ..
        } => {
            assert_eq!((accepted, requested), (4, 3));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

/// Two commits from the same revision: the first succeeds, the stale one
/// reports a conflict; failed persists retain the dirty session for retry
/// (spec A4).
#[test]
fn commit_conflict_validation_and_persist_failure_are_explicit() {
    let gates = Arc::new(GateRegistry::default());
    let store = instant_store();
    let (manager, _p, _e) = gated_manager(config(8, 1, 16, 2), &gates, Arc::clone(&store));
    let opened = manager
        .open_session(open_request("asset-a", "primary", original(2, 2, 1.0)))
        .unwrap();

    let mut env2 = envelope("asset-a", "primary", "fp-asset-a");
    env2.revision = 2;
    env2.recipe.exposure = 0.3;
    let commit = manager.commit_recipe(opened.session_id, 1, env2).unwrap();
    assert_eq!(commit.revision, 2);
    assert_eq!(store.last_revision.load(Ordering::SeqCst), 2);

    // Stale window: same expected revision again must conflict.
    let mut env3 = envelope("asset-a", "primary", "fp-asset-a");
    env3.revision = 3;
    env3.recipe.exposure = 0.4;
    match expect_err(
        manager.commit_recipe(opened.session_id, 1, env3.clone()),
        "revision conflict",
    ) {
        SessionError::RevisionConflict {
            current, expected, ..
        } => {
            assert_eq!((current, expected), (2, 1));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // Envelope identity mismatch is explicit input validation.
    let mut foreign = envelope("asset-b", "primary", "fp-asset-b");
    foreign.revision = 3;
    match expect_err(
        manager.commit_recipe(opened.session_id, 2, foreign),
        "identity mismatch rejection",
    ) {
        SessionError::InvalidInput(_) => {}
        other => panic!("unexpected error: {other:?}"),
    }

    // Non-finite recipe values are rejected by schema validation.
    let mut invalid = envelope("asset-a", "primary", "fp-asset-a");
    invalid.revision = 3;
    invalid.recipe.exposure = f64::NAN;
    match expect_err(
        manager.commit_recipe(opened.session_id, 2, invalid),
        "recipe validation rejection",
    ) {
        SessionError::InvalidInput(_) => {}
        other => panic!("unexpected error: {other:?}"),
    }

    // Persist failure: typed error, session keeps its committed state.
    store.fail.store(true, Ordering::SeqCst);
    let mut env3b = env3.clone();
    env3b.recipe.exposure = 0.6;
    match expect_err(
        manager.commit_recipe(opened.session_id, 2, env3b),
        "persist failure",
    ) {
        SessionError::Persist(message) => assert_eq!(message, "simulated disk full"),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(manager.session_info(opened.session_id).unwrap().revision, 2);
    assert_eq!(
        manager
            .session_envelope(opened.session_id)
            .unwrap()
            .recipe
            .exposure,
        0.3
    );

    // Retry after the store recovers.
    store.fail.store(false, Ordering::SeqCst);
    let retry = manager.commit_recipe(opened.session_id, 2, env3).unwrap();
    assert_eq!(retry.revision, 3);
}

/// Preview cancellation (whole-session) cannot cancel a durable save that is
/// already acknowledged to be in flight (acceptance criterion 3).
#[test]
fn preview_cannot_cancel_or_disturb_durable_saves() {
    let gates = Arc::new(GateRegistry::default());
    let (store, store_started_rx, store_gate_tx) = gated_store();
    let (p_tx, p_rx) = mpsc::channel();
    let (e_tx, _e_rx) = mpsc::channel();
    // This renderer honors cancellation after its gate: the cooperative
    // preview stops, while the durable save proceeds untouched.
    let manager = SessionManager::new(
        config(8, 1, 16, 2),
        Arc::new(GatedPreviewRenderer {
            started: p_tx,
            gates: Arc::clone(&gates),
            honor_cancel_after_gate: true,
        }),
        Arc::new(GatedExportRenderer {
            started: e_tx,
            gates: Arc::clone(&gates),
        }),
        store,
    )
    .expect("manager construction must succeed");
    let manager = Arc::new(manager);
    let opened = manager
        .open_session(open_request("asset-a", "primary", original(2, 2, 1.0)))
        .unwrap();

    // Keep one preview in flight on a held gate.
    let gate1 = gates.install((opened.session_id.0, 1));
    let ticket1 = manager
        .render_preview(PreviewRequest {
            session_id: opened.session_id,
            generation: 1,
            recipe: envelope("asset-a", "primary", "fp-asset-a"),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        })
        .unwrap();
    wait_started(&p_rx, (opened.session_id.0, 1));

    // Start a durable save; it blocks inside the store.
    let mut env2 = envelope("asset-a", "primary", "fp-asset-a");
    env2.revision = 2;
    let commit_handle = {
        let manager = Arc::clone(&manager);
        let env = env2.clone();
        std::thread::spawn(move || manager.commit_recipe(opened.session_id, 1, env).unwrap())
    };
    wait_store_started(&store_started_rx, 2);

    // Cancel ALL preview work; the save must be untouched.
    let cancelled = manager.cancel_session_previews(opened.session_id);
    assert_eq!(cancelled, 1, "the in-flight preview is cancelled");
    let _ = gate1.send(());
    match await_preview(ticket1) {
        PreviewOutcome::Cancelled { .. } => {}
        other => panic!("preview should be cancelled, got {other:?}"),
    }
    assert!(
        !commit_handle.is_finished(),
        "durable save must still be in flight after preview cancellation"
    );

    // The save settles successfully.
    let _ = store_gate_tx.send(());
    let result = commit_handle.join().expect("commit thread must not panic");
    assert_eq!(result.revision, 2);
    let diag = manager.diagnostics();
    assert_eq!(diag.commits_ok, 1);
    assert_eq!(diag.previews_cancelled, 1);
}

/// Export uses its own bounded queue over immutable snapshots; preview
/// cancellation and session closure never cancel durable export work, and
/// later commits do not change an already-enqueued export (acceptance
/// criteria 2 and 3).
#[test]
fn export_queue_is_bounded_immutable_and_independent_of_previews_and_close() {
    let gates = Arc::new(GateRegistry::default());
    let (manager, _p, e_rx) = gated_manager(config(8, 1, 16, 2), &gates, instant_store());

    let opened_a = manager
        .open_session(open_request("asset-a", "primary", original(2, 2, 10.0)))
        .unwrap();
    let opened_b = manager
        .open_session(open_request("asset-b", "primary", original(2, 2, 20.0)))
        .unwrap();
    let opened_c = manager
        .open_session(open_request("asset-c", "primary", original(2, 2, 30.0)))
        .unwrap();

    let mut env2 = envelope("asset-a", "primary", "fp-asset-a");
    env2.revision = 2;
    env2.recipe.exposure = 0.7;
    manager.commit_recipe(opened_a.session_id, 1, env2).unwrap();

    // Export A: in flight on a held gate, snapshot at revision 2.
    let gate_a = gates.install((1, 2));
    let ticket_a = manager
        .export_from_session(opened_a.session_id, None)
        .unwrap();
    assert_eq!(ticket_a.job.revision, 2);
    wait_started(&e_rx, (1, 2));

    // Export B: queued (the export queue bound is 1).
    let ticket_b = manager
        .export_from_session(opened_b.session_id, None)
        .unwrap();
    assert_eq!(manager.diagnostics().queued_exports, 1);

    // Export C: rejected — bounded queue.
    match expect_err(
        manager.export_from_session(opened_c.session_id, None),
        "export queue rejection",
    ) {
        SessionError::ExportQueueFull { limit } => assert_eq!(limit, 1),
        other => panic!("unexpected error: {other:?}"),
    }

    // Cancelling the queued export B is explicit and does not touch A.
    assert!(manager.cancel_export(ticket_b.job.job_id));
    match await_export(ticket_b) {
        ExportOutcome::Cancelled { info } => assert_eq!(info.asset_id, "asset-b"),
        other => panic!("queued export should be cancelled, got {other:?}"),
    }

    // Now C fits.
    let ticket_c = manager
        .export_from_session(opened_c.session_id, None)
        .unwrap();

    // A later commit does not change the in-flight export's immutable input.
    let mut env3 = envelope("asset-a", "primary", "fp-asset-a");
    env3.revision = 3;
    env3.recipe.exposure = 1.2;
    manager.commit_recipe(opened_a.session_id, 2, env3).unwrap();

    // Closing session A does not cancel its durable export.
    manager.close_session(opened_a.session_id).unwrap();

    let _ = gate_a.send(());
    match await_export(ticket_a) {
        ExportOutcome::Completed { info, frame, .. } => {
            assert_eq!(info.asset_id, "asset-a");
            assert_eq!(
                info.revision, 2,
                "export must use the snapshot revision, not the later commit"
            );
            assert_eq!(frame.width, 2);
            assert_eq!(
                frame.rgba8[0], 2,
                "frame payload reflects the snapshot revision"
            );
        }
        other => panic!("durable export must complete despite close, got {other:?}"),
    }
    let _ = gates.install((ticket_c.job.job_id.0, 1)).send(());
    match await_export(ticket_c) {
        ExportOutcome::Completed { info, .. } => assert_eq!(info.asset_id, "asset-c"),
        other => panic!("export C should complete, got {other:?}"),
    }

    let diag = manager.diagnostics();
    assert_eq!(diag.exports_executed, 2);
    assert_eq!(diag.exports_cancelled, 1);
    assert_eq!(diag.queued_exports, 0);
    assert_eq!(diag.in_flight_exports, 0);
}

/// Close releases resources only after pending saves settle; a closing
/// session rejects new work; repeated close is NotFound (acceptance
/// criterion 4).
#[test]
fn close_releases_resources_after_pending_saves_settle() {
    let gates = Arc::new(GateRegistry::default());
    let (store, store_started_rx, store_gate_tx) = gated_store();
    let (manager, _p, _e) = gated_manager(config(8, 1, 16, 2), &gates, store);
    let manager = Arc::new(manager);
    let orig = original(2, 2, 5.0);
    let opened = manager
        .open_session(open_request("asset-a", "primary", Arc::clone(&orig)))
        .unwrap();

    // In-flight save blocking inside the store.
    let mut env2 = envelope("asset-a", "primary", "fp-asset-a");
    env2.revision = 2;
    let commit_handle = {
        let manager = Arc::clone(&manager);
        let env = env2.clone();
        std::thread::spawn(move || manager.commit_recipe(opened.session_id, 1, env).unwrap())
    };
    wait_store_started(&store_started_rx, 2);

    // Close blocks while the save is pending.
    let close_handle = {
        let manager = Arc::clone(&manager);
        std::thread::spawn(move || manager.close_session(opened.session_id).unwrap())
    };
    for _ in 0..10 {
        assert!(
            !close_handle.is_finished(),
            "close must wait for pending saves to settle"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // New work on the closing session is rejected.
    match expect_err(
        manager.render_preview(PreviewRequest {
            session_id: opened.session_id,
            generation: 1,
            recipe: envelope("asset-a", "primary", "fp-asset-a"),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        }),
        "previews rejected while closing",
    ) {
        SessionError::SessionClosed { .. } => {}
        other => panic!("unexpected error: {other:?}"),
    }
    match expect_err(
        manager.commit_recipe(opened.session_id, 1, env2),
        "commits rejected while closing",
    ) {
        SessionError::SessionClosed { .. } => {}
        other => panic!("unexpected error: {other:?}"),
    }

    // Save settles; close completes and releases everything.
    let _ = store_gate_tx.send(());
    let commit = commit_handle.join().unwrap();
    assert_eq!(commit.revision, 2);
    let closed = close_handle.join().unwrap();
    assert_eq!(closed.session_id, opened.session_id);
    assert_eq!(closed.final_revision, 2);

    assert!(manager.session_info(opened.session_id).is_none());
    match expect_err(
        manager.close_session(opened.session_id),
        "second close NotFound",
    ) {
        SessionError::NotFound { .. } => {}
        other => panic!("unexpected error: {other:?}"),
    }

    let diag = manager.diagnostics();
    assert_eq!(diag.open_sessions, 0);
    assert_eq!(diag.tracked_original_bytes, 0);
    assert_eq!(diag.cached_preview_bytes, 0);
    assert_eq!(diag.sessions_closed, 1);
    assert_eq!(
        Arc::strong_count(&orig),
        1,
        "session must release the original buffer handle"
    );
}

/// The session cache is bounded and evicts least-recently-used idle sessions;
/// busy sessions are never evicted (bounded cache policy).
#[test]
fn session_cache_is_bounded_with_lru_eviction_of_idle_sessions() {
    let gates = Arc::new(GateRegistry::default());
    let (manager, _p_rx, _e) = gated_manager(config(2, 1, 16, 2), &gates, instant_store());

    let opened_a = manager
        .open_session(open_request("asset-a", "primary", original(2, 2, 1.0)))
        .unwrap();
    let opened_b = manager
        .open_session(open_request("asset-b", "primary", original(2, 2, 2.0)))
        .unwrap();

    // Touch A so B becomes least-recently used.
    let _ = manager.session_info(opened_a.session_id).unwrap();

    let opened_c = manager
        .open_session(open_request("asset-c", "primary", original(2, 2, 3.0)))
        .unwrap();
    assert!(
        manager.session_info(opened_b.session_id).is_none(),
        "B was idle LRU and must be evicted"
    );
    assert!(manager.session_info(opened_a.session_id).is_some());
    assert!(manager.session_info(opened_c.session_id).is_some());
    assert_eq!(manager.diagnostics().session_evictions, 1);
    assert_eq!(manager.diagnostics().open_sessions, 2);

    // Busy sessions are never evicted: A and B hold in-flight previews
    // concurrently (two preview workers so both actually start).
    let gates2 = Arc::new(GateRegistry::default());
    let (manager2, p_rx2, _e2) = gated_manager(config(2, 2, 16, 2), &gates2, instant_store());
    let busy_a = manager2
        .open_session(open_request("asset-a", "primary", original(2, 2, 1.0)))
        .unwrap();
    let busy_b = manager2
        .open_session(open_request("asset-b", "primary", original(2, 2, 2.0)))
        .unwrap();
    let gate_a = gates2.install((busy_a.session_id.0, 1));
    let gate_b = gates2.install((busy_b.session_id.0, 1));
    let t1 = manager2
        .render_preview(PreviewRequest {
            session_id: busy_a.session_id,
            generation: 1,
            recipe: envelope("asset-a", "primary", "fp-asset-a"),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        })
        .unwrap();
    let t2 = manager2
        .render_preview(PreviewRequest {
            session_id: busy_b.session_id,
            generation: 1,
            recipe: envelope("asset-b", "primary", "fp-asset-b"),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        })
        .unwrap();
    wait_started(&p_rx2, (busy_a.session_id.0, 1));
    wait_started(&p_rx2, (busy_b.session_id.0, 1));

    match expect_err(
        manager2.open_session(open_request("asset-c", "primary", original(2, 2, 3.0))),
        "no eviction of busy sessions",
    ) {
        SessionError::TooManySessions { limit } => assert_eq!(limit, 2),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = gate_a.send(());
    let _ = gate_b.send(());
    assert!(matches!(
        await_preview(t1),
        PreviewOutcome::Completed { .. }
    ));
    assert!(matches!(
        await_preview(t2),
        PreviewOutcome::Completed { .. }
    ));
}

/// The preview queue is bounded across sessions; overflow is an explicit
/// rejection (bounded queue policy).
#[test]
fn preview_queue_rejects_when_full() {
    let gates = Arc::new(GateRegistry::default());
    let (manager, p_rx, _e) = gated_manager(config(8, 1, 1, 2), &gates, instant_store());

    let opened_a = manager
        .open_session(open_request("asset-a", "primary", original(2, 2, 1.0)))
        .unwrap();
    let opened_b = manager
        .open_session(open_request("asset-b", "primary", original(2, 2, 2.0)))
        .unwrap();
    let opened_c = manager
        .open_session(open_request("asset-c", "primary", original(2, 2, 3.0)))
        .unwrap();

    let request = |session: rapidraw_develop::session::SessionId, asset: &str, generation: u64| {
        PreviewRequest {
            session_id: session,
            generation,
            recipe: envelope(asset, "primary", &format!("fp-{asset}")),
            quality: PreviewQuality::Settled,
            max_edge: 512,
        }
    };

    let gate_a = gates.install((opened_a.session_id.0, 1));
    let ticket_a = manager
        .render_preview(request(opened_a.session_id, "asset-a", 1))
        .unwrap();
    wait_started(&p_rx, (opened_a.session_id.0, 1));

    // One queued job fills the queue (bound 1).
    let ticket_b = manager
        .render_preview(request(opened_b.session_id, "asset-b", 1))
        .unwrap();

    match expect_err(
        manager.render_preview(request(opened_c.session_id, "asset-c", 1)),
        "preview queue rejection",
    ) {
        SessionError::PreviewQueueFull { limit } => assert_eq!(limit, 1),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = gate_a.send(());
    assert!(matches!(
        await_preview(ticket_a),
        PreviewOutcome::Completed { .. }
    ));
    assert!(matches!(
        await_preview(ticket_b),
        PreviewOutcome::Completed { .. }
    ));

    // Space again: C is accepted now.
    let ticket_c = manager
        .render_preview(request(opened_c.session_id, "asset-c", 1))
        .unwrap();
    assert!(matches!(
        await_preview(ticket_c),
        PreviewOutcome::Completed { .. }
    ));
}

/// Stress: open/render/coalesce-cancel/commit/close loops over two assets
/// keep every bounded counter at a stable ceiling and release all handles
/// (acceptance criterion 4, spec A12).
#[test]
fn stress_navigation_loops_keep_memory_ceilings_and_release_handles() {
    let gates = Arc::new(GateRegistry::default());
    let store = instant_store();
    let cfg = config(3, 2, 16, 2);
    let (p_tx, p_rx) = mpsc::channel();
    let (e_tx, _e_rx) = mpsc::channel();
    let manager = SessionManager::new(
        cfg,
        Arc::new(GatedPreviewRenderer {
            started: p_tx,
            gates: Arc::clone(&gates),
            honor_cancel_after_gate: false,
        }),
        Arc::new(GatedExportRenderer {
            started: e_tx,
            gates: Arc::clone(&gates),
        }),
        store,
    )
    .expect("manager construction must succeed");

    let orig_a = original(4, 4, 10.0);
    let orig_b = original(4, 4, 20.0);
    let per_original_bytes = 4 * 4 * 3 * std::mem::size_of::<f32>();
    let ceiling_original_bytes = 3 * per_original_bytes; // max_sessions = 3
    let mut unclosed: Vec<rapidraw_develop::session::SessionId> = Vec::new();

    for i in 0..150_u64 {
        let asset = if i % 2 == 0 { "asset-a" } else { "asset-b" };
        let orig = if i % 2 == 0 {
            Arc::clone(&orig_a)
        } else {
            Arc::clone(&orig_b)
        };
        let opened = manager
            .open_session(open_request(asset, "primary", orig))
            .unwrap();

        let request = |generation: u64, quality: PreviewQuality, max_edge: u32| PreviewRequest {
            session_id: opened.session_id,
            generation,
            recipe: envelope(asset, "primary", &format!("fp-{asset}")),
            quality,
            max_edge,
        };

        // gen 1 has no gate: it completes immediately.
        let settled = manager
            .render_preview(request(1, PreviewQuality::Settled, 512))
            .unwrap();
        assert!(matches!(
            await_preview(settled),
            PreviewOutcome::Completed { .. }
        ));

        // Deterministic coalescing: hold gen 2 in flight on a gate, then
        // accept gen 3 and release the gate. The renderer completes the
        // obsolete job regardless; the manager must drop it as stale.
        let gate2 = gates.install((opened.session_id.0, 2));
        let stale = manager
            .render_preview(request(2, PreviewQuality::Interactive, 256))
            .unwrap();
        wait_started(&p_rx, (opened.session_id.0, 2));
        let current = manager
            .render_preview(request(3, PreviewQuality::Interactive, 256))
            .unwrap();
        let _ = gate2.send(());
        match await_preview(stale) {
            PreviewOutcome::Cancelled { info } => assert_eq!(info.generation, 2),
            other => panic!("stale gen 2 must be dropped, got {other:?}"),
        }
        assert!(matches!(
            await_preview(current),
            PreviewOutcome::Completed { .. }
        ));

        if i % 4 == 0 {
            let mut env = envelope(asset, "primary", &format!("fp-{asset}"));
            env.revision = 2;
            env.recipe.exposure = (i % 10) as f64 * 0.01;
            manager.commit_recipe(opened.session_id, 1, env).unwrap();
        }

        if i % 10 == 9 {
            unclosed.push(opened.session_id);
        } else {
            manager.close_session(opened.session_id).unwrap();
        }

        // Hold at most the configured number of sessions.
        while unclosed.len() >= cfg.max_sessions {
            let id = unclosed.remove(0);
            manager.close_session(id).unwrap();
        }

        let diag = manager.diagnostics();
        assert!(
            diag.open_sessions <= cfg.max_sessions,
            "session ceiling violated: {diag:?}"
        );
        assert!(
            diag.tracked_original_bytes <= ceiling_original_bytes,
            "original-buffer ceiling violated: {diag:?}"
        );
        if unclosed.is_empty() {
            assert_eq!(diag.open_sessions, 0);
            assert_eq!(diag.tracked_original_bytes, 0);
            assert_eq!(diag.cached_preview_entries, 0);
            assert_eq!(diag.cached_preview_bytes, 0);
        }
    }

    for id in unclosed {
        manager.close_session(id).unwrap();
    }
    let diag = manager.diagnostics();
    assert_eq!(diag.open_sessions, 0);
    assert_eq!(diag.tracked_original_bytes, 0);
    assert_eq!(diag.cached_preview_bytes, 0);
    assert_eq!(diag.sessions_opened, 150);
    assert_eq!(
        diag.sessions_opened,
        diag.sessions_closed + diag.session_evictions
    );
    // gen 1 and gen 3 execute per iteration; gen 2 executes but is stale.
    assert_eq!(diag.previews_executed, 450);
    assert_eq!(diag.previews_dropped_stale, 150);
    assert_eq!(diag.commits_ok, 38);
    assert_eq!(Arc::strong_count(&orig_a), 1);
    assert_eq!(Arc::strong_count(&orig_b), 1);
}
