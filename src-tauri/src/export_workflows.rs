//! Serializable contracts, discovery registry, and the bounded subprocess
//! runner for trusted local export workflows.
//!
//! This module is the normative Rust model for workflow protocol v1, specified
//! in `docs/decisions/export-workflow-protocol.md`, plus the registry that
//! scans the bundled `resources/workflows` directory and
//! `~/.rapidraw/workflows` for `.py`/`.js` files, probes interpreter
//! availability, and the [`CommandWorkflowRunner`] that launches one workflow
//! invocation directly (never through a shell) with finite timeouts, bounded
//! output, a minimal environment, and tree-wide termination on cancellation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize};
use tauri::Emitter;
use tauri::Manager;

pub const WORKFLOW_PROTOCOL_VERSION: u32 = 1;

/// Bound on a single interpreter probe during discovery.
pub const WORKFLOW_DISCOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Cached discovery results are reused within this window unless explicitly refreshed.
pub const WORKFLOW_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(30);
const WORKFLOW_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const WORKFLOW_VERSION_LINE_MAX_CHARS: usize = 100;
const WORKFLOW_VERSION_LINE_MAX_BYTES: usize = 4096;
/// Suffix of the optional metadata sidecar next to a workflow file.
const WORKFLOW_SIDECAR_SUFFIX: &str = ".rapidraw.json";

/// Default sort order for workflows that do not declare one.
pub const WORKFLOW_DEFAULT_ORDER: i32 = 100;
pub const WORKFLOW_MIN_ORDER: i32 = 0;
pub const WORKFLOW_MAX_ORDER: i32 = 1000;

/// Default subprocess timeout; metadata may tighten or widen it within bounds.
pub const WORKFLOW_DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub const WORKFLOW_MIN_TIMEOUT_SECONDS: u64 = 1;
pub const WORKFLOW_MAX_TIMEOUT_SECONDS: u64 = 600;

/// Stable workflow ids are ASCII `[a-z0-9-]`, starting alphanumeric, at most this long.
pub const WORKFLOW_ID_MAX_CHARS: usize = 64;
const WORKFLOW_DISPLAY_NAME_MAX_CHARS: usize = 100;
const WORKFLOW_DESCRIPTION_MAX_CHARS: usize = 500;

fn deserialize_protocol_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == WORKFLOW_PROTOCOL_VERSION {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported workflow protocol version {version}; expected {WORKFLOW_PROTOCOL_VERSION}"
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowLanguage {
    Python,
    JavaScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowPhase {
    PostImage,
    PostBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowSource {
    Bundled,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowErrorPolicy {
    Warn,
    Fail,
}

/// Optional sidecar `<script>.rapidraw.json` next to a workflow file.
///
/// Every field is optional; a direct `.py`/`.js` file with no sidecar remains valid with
/// defaults derived from its file name and extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMetadata {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub phase: Option<WorkflowPhase>,
    pub order: Option<i32>,
    pub timeout_seconds: Option<u64>,
    pub on_error: Option<WorkflowErrorPolicy>,
}

impl WorkflowMetadata {
    /// Returns typed diagnostics for out-of-contract values; empty means the sidecar is valid.
    pub fn validate(&self) -> Vec<WorkflowDiagnostic> {
        let mut diagnostics = Vec::new();
        if let Some(id) = &self.id
            && !is_valid_workflow_id(id)
        {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.metadata.id.invalid".to_string(),
                message: format!(
                    "id must be 1..={WORKFLOW_ID_MAX_CHARS} chars of [a-z0-9-] starting alphanumeric"
                ),
            });
        }
        if let Some(display_name) = &self.display_name
            && !is_valid_display_name(display_name)
        {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.metadata.displayName.tooLong".to_string(),
                message: format!(
                    "displayName must be at most {WORKFLOW_DISPLAY_NAME_MAX_CHARS} characters"
                ),
            });
        }
        if let Some(description) = &self.description
            && !is_valid_description(description)
        {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.metadata.description.tooLong".to_string(),
                message: format!(
                    "description must be at most {WORKFLOW_DESCRIPTION_MAX_CHARS} characters"
                ),
            });
        }
        if let Some(order) = self.order
            && !is_valid_order(order)
        {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.metadata.order.outOfRange".to_string(),
                message: format!("order must be {WORKFLOW_MIN_ORDER}..={WORKFLOW_MAX_ORDER}"),
            });
        }
        if let Some(timeout_seconds) = self.timeout_seconds
            && !is_valid_timeout_seconds(timeout_seconds)
        {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.metadata.timeoutSeconds.outOfRange".to_string(),
                message: format!(
                    "timeoutSeconds must be {WORKFLOW_MIN_TIMEOUT_SECONDS}..={WORKFLOW_MAX_TIMEOUT_SECONDS}"
                ),
            });
        }
        diagnostics
    }
}

fn is_valid_workflow_id(id: &str) -> bool {
    let chars: Vec<char> = id.chars().collect();
    if chars.is_empty() || chars.len() > WORKFLOW_ID_MAX_CHARS {
        return false;
    }
    let first = chars[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
}

/// Field-level predicates shared by sidecar validation and sidecar application, so an
/// out-of-contract value never reaches a discovered workflow even when the sidecar parses.
fn is_valid_display_name(display_name: &str) -> bool {
    display_name.chars().count() <= WORKFLOW_DISPLAY_NAME_MAX_CHARS
}

fn is_valid_description(description: &str) -> bool {
    description.chars().count() <= WORKFLOW_DESCRIPTION_MAX_CHARS
}

fn is_valid_order(order: i32) -> bool {
    (WORKFLOW_MIN_ORDER..=WORKFLOW_MAX_ORDER).contains(&order)
}

fn is_valid_timeout_seconds(timeout_seconds: u64) -> bool {
    (WORKFLOW_MIN_TIMEOUT_SECONDS..=WORKFLOW_MAX_TIMEOUT_SECONDS).contains(&timeout_seconds)
}

/// Identity and behavior derived from a workflow file name alone, without any manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDefaults {
    pub id: String,
    pub display_name: String,
    pub language: WorkflowLanguage,
    pub phase: WorkflowPhase,
    pub order: i32,
    pub timeout_seconds: u64,
    pub on_error: WorkflowErrorPolicy,
}

/// Derives safe defaults from a workflow file name. Returns `None` for unsupported extensions
/// or sidecar/metadata file names.
pub fn derive_workflow_defaults(file_name: &str) -> Option<WorkflowDefaults> {
    let path = std::path::Path::new(file_name);
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let language = match extension.as_str() {
        "py" => WorkflowLanguage::Python,
        "js" => WorkflowLanguage::JavaScript,
        _ => return None,
    };
    let stem = path.file_stem()?.to_str()?;
    if stem.is_empty() {
        return None;
    }
    Some(WorkflowDefaults {
        id: normalize_workflow_id(stem),
        display_name: display_name_from_stem(stem),
        language,
        phase: WorkflowPhase::PostBatch,
        order: WORKFLOW_DEFAULT_ORDER,
        timeout_seconds: WORKFLOW_DEFAULT_TIMEOUT_SECONDS,
        on_error: WorkflowErrorPolicy::Warn,
    })
}

/// Normalizes a file stem to a stable ASCII id: lowercase, non-alphanumerics act as
/// separators, runs collapse, the result is trimmed and bounded.
fn normalize_workflow_id(stem: &str) -> String {
    let mut id = String::with_capacity(WORKFLOW_ID_MAX_CHARS);
    let mut pending_separator = false;
    for ch in stem.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            if pending_separator && !id.is_empty() {
                id.push('-');
            }
            pending_separator = false;
            id.push(lower);
            if id.len() >= WORKFLOW_ID_MAX_CHARS {
                break;
            }
        } else if !id.is_empty() {
            pending_separator = true;
        }
    }
    if id.is_empty() {
        "workflow".to_string()
    } else {
        id
    }
}

fn display_name_from_stem(stem: &str) -> String {
    let replaced = stem.replace(['-', '_'], " ");
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "workflow".to_string()
    } else {
        collapsed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRuntime {
    pub available: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDiagnostic {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredWorkflow {
    pub id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub language: WorkflowLanguage,
    pub phase: WorkflowPhase,
    pub order: i32,
    pub timeout_seconds: u64,
    pub on_error: WorkflowErrorPolicy,
    pub source: WorkflowSource,
    /// Canonical script location (verbatim prefix stripped); never a symlink and always
    /// inside its scanning root.
    pub script_path: String,
    /// False when invalid metadata or an unreadable entry makes the workflow unselectable.
    /// Runtime availability is reported separately through `runtime.available`.
    pub selectable: bool,
    pub runtime: WorkflowRuntime,
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDiscoveryResult {
    pub workflows: Vec<DiscoveredWorkflow>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSelection {
    pub workflow_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArtifact {
    pub path: String,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowItem {
    pub source_path: String,
    pub exported_path: Option<String>,
    pub artifacts: Vec<WorkflowArtifact>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExportSettings {
    pub file_format: String,
    pub jpeg_quality: u8,
    pub keep_metadata: bool,
    pub strip_gps: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRequest {
    #[serde(deserialize_with = "deserialize_protocol_version")]
    pub protocol_version: u32,
    pub run_id: String,
    pub workflow_id: String,
    pub phase: WorkflowPhase,
    pub source_path: Option<String>,
    pub exported_path: Option<String>,
    pub artifacts: Vec<WorkflowArtifact>,
    pub selected_items: Vec<WorkflowItem>,
    pub exported_items: Vec<WorkflowItem>,
    pub export_settings: WorkflowExportSettings,
    pub index: Option<u64>,
    pub total: u64,
    pub workspace_temp_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowResponse {
    pub ok: bool,
    pub message: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub produced_artifact_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowProgressPhase {
    Discovering,
    Waiting,
    Rendering,
    Writing,
    RunningPostImage,
    RunningPostBatch,
    Cancelling,
    Complete,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowProgressEvent {
    pub run_id: String,
    pub phase: WorkflowProgressPhase,
    pub workflow_id: Option<String>,
    pub source_path: Option<String>,
    pub index: u64,
    pub total: u64,
    pub timeout_seconds: Option<u64>,
    pub warning_count: u64,
}

/// Receives typed lifecycle events for one export run. Implementations must be
/// cheap and never block the export pipeline; events are already bounded and
/// carry counters, never raw subprocess output.
pub type WorkflowProgressSink = Arc<dyn Fn(WorkflowProgressEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRunStatus {
    Succeeded,
    Warned,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunResult {
    pub workflow_id: String,
    pub source_path: Option<String>,
    pub status: WorkflowRunStatus,
    pub message: Option<String>,
    pub warnings: Vec<String>,
    pub produced_artifacts: Vec<WorkflowArtifact>,
    /// Bounded, redacted stderr excerpt captured for this invocation; never
    /// raw or unbounded subprocess output.
    #[serde(default)]
    pub stderr_excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowBatchResult {
    pub run_id: String,
    pub cancelled: bool,
    pub results: Vec<WorkflowRunResult>,
}

/// One settled image of an export: its exported path on success, or the exact
/// error that failed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportItemOutcome {
    pub source_path: String,
    pub exported_path: Option<String>,
    pub error: Option<String>,
}

/// Terminal export detail forwarded to the UI: per-image and per-workflow
/// outcomes with counts, so a result view can show exactly what happened
/// instead of only "N of M failed".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResultDetail {
    pub run_id: String,
    pub cancelled: bool,
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub warned_workflow_runs: u64,
    pub failed_workflow_runs: u64,
    pub items: Vec<ExportItemOutcome>,
    pub workflow_runs: Vec<WorkflowRunResult>,
}

/// Builds the terminal [`ExportResultDetail`]. `succeeded` counts items
/// without an error; `failed` counts items with one (a cancelled run keeps its
/// per-item errors and sets `cancelled`). Warned workflow runs never count as
/// failures — only `Failed`/`TimedOut` runs do.
pub fn summarize_export_results(
    run_id: &str,
    cancelled: bool,
    items: Vec<ExportItemOutcome>,
    workflow_runs: Vec<WorkflowRunResult>,
) -> ExportResultDetail {
    let succeeded = items.iter().filter(|item| item.error.is_none()).count() as u64;
    let failed = items.len() as u64 - succeeded;
    let warned_workflow_runs = workflow_runs
        .iter()
        .filter(|run| run.status == WorkflowRunStatus::Warned)
        .count() as u64;
    let failed_workflow_runs = workflow_runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                WorkflowRunStatus::Failed | WorkflowRunStatus::TimedOut
            )
        })
        .count() as u64;
    ExportResultDetail {
        run_id: run_id.to_string(),
        cancelled,
        total: items.len() as u64,
        succeeded,
        failed,
        warned_workflow_runs,
        failed_workflow_runs,
        items,
        workflow_runs,
    }
}

/// The stream that exceeded its byte bound; both are distinct failure inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowOutputStream {
    Stdout,
    Stderr,
}

/// Distinct typed outcomes for one workflow invocation. Malformed output and a non-zero exit
/// code are separate errors: a script may exit 0 with invalid JSON, or exit non-zero after
/// writing a partial document. A response with `ok: false` is not an execution error; it is a
/// script-reported failure interpreted through the workflow's `onError` policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowExecutionError {
    MissingRuntime {
        language: WorkflowLanguage,
        reason: String,
    },
    Spawn {
        reason: String,
    },
    /// The script file no longer matches the canonical identity discovery
    /// recorded: it was deleted, replaced by a symlink/reparse point, moved,
    /// or now canonicalizes outside the workflow roots. Spawn-time
    /// revalidation rejects it before any interpreter is launched.
    ScriptIdentity {
        reason: String,
    },
    MalformedOutput {
        reason: String,
    },
    NonZeroExit {
        code: i32,
    },
    OutputLimitExceeded {
        stream: WorkflowOutputStream,
    },
    TimedOut,
    Cancelled,
}

impl WorkflowExecutionError {
    /// Maps the error onto the terminal status reported for the affected item or batch.
    pub fn status(&self) -> WorkflowRunStatus {
        match self {
            Self::TimedOut => WorkflowRunStatus::TimedOut,
            Self::Cancelled => WorkflowRunStatus::Cancelled,
            _ => WorkflowRunStatus::Failed,
        }
    }
}

impl std::fmt::Display for WorkflowExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRuntime { language, reason } => write!(
                f,
                "no {} runtime available for workflow: {reason}",
                match language {
                    WorkflowLanguage::Python => "python",
                    WorkflowLanguage::JavaScript => "javascript",
                }
            ),
            Self::Spawn { reason } => write!(f, "failed to launch workflow process: {reason}"),
            Self::ScriptIdentity { reason } => write!(
                f,
                "workflow script no longer matches its discovered identity: {reason}"
            ),
            Self::MalformedOutput { reason } => {
                write!(f, "malformed workflow output: {reason}")
            }
            Self::NonZeroExit { code } => {
                write!(f, "workflow exited with non-zero exit code {code}")
            }
            Self::OutputLimitExceeded { stream } => write!(
                f,
                "workflow exceeded the {} byte limit",
                match stream {
                    WorkflowOutputStream::Stdout => "stdout",
                    WorkflowOutputStream::Stderr => "stderr",
                }
            ),
            Self::TimedOut => write!(f, "workflow exceeded its timeout"),
            Self::Cancelled => write!(f, "workflow was cancelled"),
        }
    }
}

impl std::error::Error for WorkflowExecutionError {}

/// Platform used to order interpreter probe candidates; injectable so Windows and Unix
/// ordering can be tested on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbePlatform {
    Windows,
    Unix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCandidate {
    pub program: String,
    pub args: Vec<String>,
}

impl RuntimeCandidate {
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }
}

/// Python candidates: the `py -3` launcher first on Windows, `python3` first elsewhere.
pub fn python_runtime_candidates(platform: ProbePlatform) -> Vec<RuntimeCandidate> {
    match platform {
        ProbePlatform::Windows => vec![
            RuntimeCandidate {
                program: "py".to_string(),
                args: vec!["-3".to_string(), "--version".to_string()],
            },
            RuntimeCandidate {
                program: "python".to_string(),
                args: vec!["--version".to_string()],
            },
        ],
        ProbePlatform::Unix => vec![
            RuntimeCandidate {
                program: "python3".to_string(),
                args: vec!["--version".to_string()],
            },
            RuntimeCandidate {
                program: "python".to_string(),
                args: vec!["--version".to_string()],
            },
        ],
    }
}

pub fn node_runtime_candidates() -> Vec<RuntimeCandidate> {
    vec![RuntimeCandidate {
        program: "node".to_string(),
        args: vec!["--version".to_string()],
    }]
}

/// Spawns one candidate program with an argument array and bounds its runtime. Probing never
/// goes through a shell; the timeout is enforced by polling and killing the child.
pub trait RuntimeProber: Send + Sync {
    fn probe(&self, candidate: &RuntimeCandidate) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct CommandRuntimeProber {
    pub timeout: Duration,
}

impl Default for CommandRuntimeProber {
    fn default() -> Self {
        Self {
            timeout: WORKFLOW_DISCOVERY_PROBE_TIMEOUT,
        }
    }
}

impl RuntimeProber for CommandRuntimeProber {
    fn probe(&self, candidate: &RuntimeCandidate) -> Result<String, String> {
        let label = candidate.display();
        let mut child = Command::new(&candidate.program)
            .args(&candidate.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not launch '{label}': {error}"))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "'{label}' did not respond within {} ms",
                            self.timeout.as_millis()
                        ));
                    }
                    std::thread::sleep(WORKFLOW_PROBE_POLL_INTERVAL);
                }
                Err(error) => return Err(format!("could not launch '{label}': {error}")),
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("could not read '{label}' output: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "'{label}' exited with {}",
                output.status.code().unwrap_or(-1)
            ));
        }
        first_version_line(&output.stdout)
            .or_else(|| first_version_line(&output.stderr))
            .ok_or_else(|| format!("'{label}' produced no version output"))
    }
}

fn first_version_line(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(bytes.len())
        .min(WORKFLOW_VERSION_LINE_MAX_BYTES);
    let line = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(WORKFLOW_VERSION_LINE_MAX_CHARS).collect())
    }
}

/// Inputs to one discovery run. Roots are `None` when the host cannot resolve them; missing
/// directories are reported as diagnostics rather than failures.
pub struct WorkflowDiscoveryOptions<'a> {
    pub bundled_root: Option<PathBuf>,
    pub user_root: Option<PathBuf>,
    pub platform: ProbePlatform,
    pub prober: &'a dyn RuntimeProber,
}

fn probe_runtime_language(
    language: WorkflowLanguage,
    platform: ProbePlatform,
    prober: &dyn RuntimeProber,
) -> WorkflowRuntime {
    let candidates = match language {
        WorkflowLanguage::Python => python_runtime_candidates(platform),
        WorkflowLanguage::JavaScript => node_runtime_candidates(),
    };
    let mut reasons = Vec::new();
    for candidate in &candidates {
        match prober.probe(candidate) {
            Ok(version) => {
                return WorkflowRuntime {
                    available: true,
                    executable: Some(candidate.program.clone()),
                    version: Some(version),
                    unavailable_reason: None,
                };
            }
            Err(reason) => reasons.push(format!("'{}': {reason}", candidate.display())),
        }
    }
    WorkflowRuntime {
        available: false,
        executable: None,
        version: None,
        unavailable_reason: Some(reasons.join("; ")),
    }
}

/// Canonical-path containment. Component-wise, so a sibling like `workflows-2` never matches
/// `workflows`; both sides must already be canonical (or share the same verbatim form).
fn path_is_within(child: &Path, ancestor: &Path) -> bool {
    child.starts_with(ancestor)
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string())
}

enum SidecarRead {
    None,
    Invalid,
    Ok(WorkflowMetadata),
}

fn read_sidecar(
    canonical_root: &Path,
    script_name: &str,
    sidecar_names: &HashMap<String, String>,
    source_label: &str,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> SidecarRead {
    let lookup = format!(
        "{}{}",
        script_name.to_ascii_lowercase(),
        WORKFLOW_SIDECAR_SUFFIX
    );
    let Some(actual_name) = sidecar_names.get(&lookup) else {
        return SidecarRead::None;
    };
    let sidecar_path = canonical_root.join(actual_name);
    let metadata = std::fs::symlink_metadata(&sidecar_path);
    if let Ok(meta) = &metadata
        && meta.file_type().is_symlink()
    {
        diagnostics.push(WorkflowDiagnostic {
            code: "workflow.discovery.symlink.rejected".to_string(),
            message: format!(
                "metadata sidecar for '{script_name}' in the {source_label} root is a symlink and was ignored: {}",
                sidecar_path.display()
            ),
        });
        return SidecarRead::Invalid;
    }
    let canonical = sidecar_path.canonicalize();
    match canonical {
        Ok(canonical) if !path_is_within(&canonical, canonical_root) => {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.path.escape".to_string(),
                message: format!(
                    "metadata sidecar for '{script_name}' escapes the {source_label} root and was ignored: {}",
                    canonical.display()
                ),
            });
            SidecarRead::Invalid
        }
        Ok(canonical) => match std::fs::read_to_string(&canonical) {
            Ok(text) => match serde_json::from_str::<WorkflowMetadata>(&text) {
                Ok(metadata) => SidecarRead::Ok(metadata),
                Err(error) => {
                    diagnostics.push(WorkflowDiagnostic {
                        code: "workflow.discovery.metadata.invalid".to_string(),
                        message: format!(
                            "metadata sidecar for '{script_name}' is not valid JSON and was ignored: {error}"
                        ),
                    });
                    SidecarRead::Invalid
                }
            },
            Err(error) => {
                diagnostics.push(WorkflowDiagnostic {
                    code: "workflow.discovery.metadata.unreadable".to_string(),
                    message: format!(
                        "metadata sidecar for '{script_name}' could not be read and was ignored: {error}"
                    ),
                });
                SidecarRead::Invalid
            }
        },
        Err(error) => {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.metadata.unreadable".to_string(),
                message: format!(
                    "metadata sidecar for '{script_name}' could not be resolved and was ignored: {error}"
                ),
            });
            SidecarRead::Invalid
        }
    }
}

struct ScannedWorkflow {
    workflow: DiscoveredWorkflow,
    canonical_script_path: PathBuf,
}

/// Scans exactly one directory level of a workflow root. Never executes or reads workflow
/// file contents; only names, file types, canonical paths, and sidecar metadata are used.
#[allow(clippy::too_many_arguments)]
fn scan_workflow_root(
    root: Option<&Path>,
    source: WorkflowSource,
    source_label: &str,
    platform: ProbePlatform,
    prober: &dyn RuntimeProber,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
    runtime_cache: &mut HashMap<WorkflowLanguage, WorkflowRuntime>,
    unavailable_reported: &mut HashSet<WorkflowLanguage>,
) -> Vec<ScannedWorkflow> {
    let Some(root) = root else {
        diagnostics.push(WorkflowDiagnostic {
            code: "workflow.discovery.root.missing".to_string(),
            message: format!("the {source_label} workflows root could not be resolved"),
        });
        return Vec::new();
    };
    if !root.exists() {
        diagnostics.push(WorkflowDiagnostic {
            code: "workflow.discovery.root.missing".to_string(),
            message: format!(
                "the {source_label} workflows root does not exist: {}",
                root.display()
            ),
        });
        return Vec::new();
    }
    let canonical_root = match root.canonicalize() {
        Ok(canonical) => canonical,
        Err(error) => {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.root.unreadable".to_string(),
                message: format!(
                    "the {source_label} workflows root could not be canonicalized: {error}"
                ),
            });
            return Vec::new();
        }
    };
    let entries = std::fs::read_dir(&canonical_root).map_err(|error| {
        diagnostics.push(WorkflowDiagnostic {
            code: "workflow.discovery.root.unreadable".to_string(),
            message: format!("the {source_label} workflows root could not be read: {error}"),
        });
    });
    let mut raw: Vec<(String, PathBuf, std::fs::FileType)> = Vec::new();
    if let Ok(entries) = entries {
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    diagnostics.push(WorkflowDiagnostic {
                        code: "workflow.discovery.root.unreadable".to_string(),
                        message: format!(
                            "an entry of the {source_label} workflows root could not be read: {error}"
                        ),
                    });
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    diagnostics.push(WorkflowDiagnostic {
                        code: "workflow.discovery.file.unreadable".to_string(),
                        message: format!(
                            "'{name}' in the {source_label} root could not be inspected: {error}"
                        ),
                    });
                    continue;
                }
            };
            raw.push((name, entry.path(), file_type));
        }
    }
    raw.sort_by(|a, b| a.0.cmp(&b.0));

    struct PendingWorkflow {
        name: String,
        path: PathBuf,
    }
    let mut sidecar_names: HashMap<String, String> = HashMap::new();
    let mut pending: Vec<PendingWorkflow> = Vec::new();
    for (name, path, file_type) in raw {
        if file_type.is_dir() {
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        if name.to_ascii_lowercase().ends_with(WORKFLOW_SIDECAR_SUFFIX) {
            sidecar_names.insert(name.to_ascii_lowercase(), name);
            continue;
        }
        if file_type.is_symlink() {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.symlink.rejected".to_string(),
                message: format!(
                    "symlink workflow entries in the {source_label} root are rejected: {name}"
                ),
            });
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let lowered = name.to_ascii_lowercase();
        if lowered.ends_with(".py") || lowered.ends_with(".js") {
            pending.push(PendingWorkflow { name, path });
        } else {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.extension.unsupported".to_string(),
                message: format!(
                    "file '{name}' in the {source_label} root is not a .py or .js workflow and was ignored"
                ),
            });
        }
    }

    let mut scanned: Vec<ScannedWorkflow> = Vec::new();
    for candidate in pending {
        let Some(defaults) = derive_workflow_defaults(&candidate.name) else {
            continue;
        };
        let canonical_script = match candidate.path.canonicalize() {
            Ok(canonical) => canonical,
            Err(error) => {
                diagnostics.push(WorkflowDiagnostic {
                    code: "workflow.discovery.file.unreadable".to_string(),
                    message: format!(
                        "workflow '{}' in the {source_label} root could not be resolved: {error}",
                        candidate.name
                    ),
                });
                continue;
            }
        };
        if !path_is_within(&canonical_script, &canonical_root) {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.path.escape".to_string(),
                message: format!(
                    "workflow '{}' canonicalizes outside the {source_label} root and was rejected: {}",
                    candidate.name,
                    canonical_script.display()
                ),
            });
            continue;
        }

        let mut id = defaults.id.clone();
        let mut display_name = defaults.display_name.clone();
        let mut description: Option<String> = None;
        let mut phase = defaults.phase;
        let mut order = defaults.order;
        let mut timeout_seconds = defaults.timeout_seconds;
        let mut on_error = defaults.on_error;
        let mut selectable = true;
        let mut entry_diagnostics: Vec<WorkflowDiagnostic> = Vec::new();
        match read_sidecar(
            &canonical_root,
            &candidate.name,
            &sidecar_names,
            source_label,
            diagnostics,
        ) {
            SidecarRead::None => {}
            SidecarRead::Invalid => selectable = false,
            SidecarRead::Ok(metadata) => {
                entry_diagnostics.extend(metadata.validate());
                if !entry_diagnostics.is_empty() {
                    selectable = false;
                }
                if let Some(sidecar_id) = &metadata.id
                    && is_valid_workflow_id(sidecar_id)
                {
                    id = sidecar_id.clone();
                }
                if let Some(name) = &metadata.display_name
                    && is_valid_display_name(name)
                {
                    display_name = name.clone();
                }
                if let Some(text) = &metadata.description
                    && is_valid_description(text)
                {
                    description = Some(text.clone());
                }
                if let Some(sidecar_phase) = metadata.phase {
                    phase = sidecar_phase;
                }
                if let Some(sidecar_order) = metadata.order
                    && is_valid_order(sidecar_order)
                {
                    order = sidecar_order;
                }
                if let Some(sidecar_timeout) = metadata.timeout_seconds
                    && is_valid_timeout_seconds(sidecar_timeout)
                {
                    timeout_seconds = sidecar_timeout;
                }
                if let Some(policy) = metadata.on_error {
                    on_error = policy;
                }
            }
        }

        let runtime = runtime_cache
            .entry(defaults.language)
            .or_insert_with(|| probe_runtime_language(defaults.language, platform, prober))
            .clone();
        if !runtime.available && unavailable_reported.insert(defaults.language) {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.runtime.unavailable".to_string(),
                message: format!(
                    "no {} runtime available: {}",
                    match defaults.language {
                        WorkflowLanguage::Python => "python",
                        WorkflowLanguage::JavaScript => "javascript",
                    },
                    runtime
                        .unavailable_reason
                        .as_deref()
                        .unwrap_or("unknown reason")
                ),
            });
        }

        scanned.push(ScannedWorkflow {
            workflow: DiscoveredWorkflow {
                id,
                display_name,
                description,
                language: defaults.language,
                phase,
                order,
                timeout_seconds,
                on_error,
                source,
                script_path: strip_verbatim(&canonical_script)
                    .to_string_lossy()
                    .to_string(),
                selectable,
                runtime,
                diagnostics: entry_diagnostics,
            },
            canonical_script_path: canonical_script,
        });
    }

    let consumed: HashSet<String> = scanned
        .iter()
        .map(|entry| {
            format!(
                "{}{}",
                Path::new(&entry.canonical_script_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default(),
                WORKFLOW_SIDECAR_SUFFIX
            )
        })
        .collect();
    for lookup in sidecar_names.keys() {
        if !consumed.contains(lookup) {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.sidecar.orphaned".to_string(),
                message: format!(
                    "metadata sidecar '{lookup}' in the {source_label} root has no matching workflow file"
                ),
            });
        }
    }

    // Within one root the lexicographically smallest canonical script path wins.
    scanned.sort_by(|a, b| a.canonical_script_path.cmp(&b.canonical_script_path));
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    for entry in scanned {
        if !seen_ids.insert(entry.workflow.id.clone()) {
            diagnostics.push(WorkflowDiagnostic {
                code: "workflow.discovery.duplicate.id".to_string(),
                message: format!(
                    "duplicate workflow id '{}' in the {source_label} root; shadowed script: {}",
                    entry.workflow.id,
                    entry.canonical_script_path.display()
                ),
            });
            continue;
        }
        result.push(entry);
    }
    result
}

/// Discovers workflows from the bundled and user roots. Never executes workflow code; probes
/// interpreters lazily, once per language per run, only when a workflow needs that language.
pub fn discover_workflows(options: &WorkflowDiscoveryOptions) -> WorkflowDiscoveryResult {
    let mut diagnostics = Vec::new();
    let mut runtime_cache: HashMap<WorkflowLanguage, WorkflowRuntime> = HashMap::new();
    let mut unavailable_reported: HashSet<WorkflowLanguage> = HashSet::new();
    let mut workflows: Vec<DiscoveredWorkflow> = scan_workflow_root(
        options.bundled_root.as_deref(),
        WorkflowSource::Bundled,
        "bundled",
        options.platform,
        options.prober,
        &mut diagnostics,
        &mut runtime_cache,
        &mut unavailable_reported,
    )
    .into_iter()
    .map(|entry| entry.workflow)
    .collect();

    let user_workflows = scan_workflow_root(
        options.user_root.as_deref(),
        WorkflowSource::User,
        "user",
        options.platform,
        options.prober,
        &mut diagnostics,
        &mut runtime_cache,
        &mut unavailable_reported,
    );
    for entry in user_workflows {
        match workflows
            .iter_mut()
            .find(|existing| existing.id == entry.workflow.id)
        {
            Some(existing) => {
                diagnostics.push(WorkflowDiagnostic {
                    code: "workflow.discovery.override.user".to_string(),
                    message: format!(
                        "user workflow '{}' overrides the bundled workflow with the same id (bundled script: {})",
                        entry.workflow.id,
                        existing.script_path
                    ),
                });
                *existing = entry.workflow;
            }
            None => workflows.push(entry.workflow),
        }
    }

    workflows.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
    diagnostics.sort_by(|a, b| a.code.cmp(&b.code).then_with(|| a.message.cmp(&b.message)));
    diagnostics.dedup_by(|a, b| a.code == b.code && a.message == b.message);
    WorkflowDiscoveryResult {
        workflows,
        diagnostics,
    }
}

/// Changes between two discovery runs, by workflow id, deterministically sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRefreshDelta {
    pub added_workflow_ids: Vec<String>,
    pub removed_workflow_ids: Vec<String>,
    pub availability_changes: Vec<String>,
}

pub fn diff_workflow_discovery(
    previous: &WorkflowDiscoveryResult,
    current: &WorkflowDiscoveryResult,
) -> WorkflowRefreshDelta {
    let previous_ids: HashMap<&str, &DiscoveredWorkflow> = previous
        .workflows
        .iter()
        .map(|workflow| (workflow.id.as_str(), workflow))
        .collect();
    let current_ids: HashMap<&str, &DiscoveredWorkflow> = current
        .workflows
        .iter()
        .map(|workflow| (workflow.id.as_str(), workflow))
        .collect();

    let mut added_workflow_ids: Vec<String> = current_ids
        .keys()
        .filter(|id| !previous_ids.contains_key(*id))
        .map(|id| id.to_string())
        .collect();
    let mut removed_workflow_ids: Vec<String> = previous_ids
        .keys()
        .filter(|id| !current_ids.contains_key(*id))
        .map(|id| id.to_string())
        .collect();
    let mut availability_changes: Vec<String> = current_ids
        .iter()
        .filter(|(id, workflow)| {
            previous_ids
                .get(*id)
                .is_some_and(|before| before.runtime != workflow.runtime)
        })
        .map(|(id, _)| id.to_string())
        .collect();
    added_workflow_ids.sort();
    removed_workflow_ids.sort();
    availability_changes.sort();
    WorkflowRefreshDelta {
        added_workflow_ids,
        removed_workflow_ids,
        availability_changes,
    }
}

/// The full discovery state plus the delta against the previously cached run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRefreshReport {
    pub result: WorkflowDiscoveryResult,
    pub added_workflow_ids: Vec<String>,
    pub removed_workflow_ids: Vec<String>,
    pub availability_changes: Vec<String>,
}

/// Internal cache entry backing the `discover_export_workflows` and
/// `refresh_export_workflows` commands.
#[derive(Debug, Clone)]
pub struct WorkflowDiscoveryCache {
    pub result: WorkflowDiscoveryResult,
    pub refreshed_at: Instant,
}

pub fn discovery_probe_platform() -> ProbePlatform {
    #[cfg(target_os = "windows")]
    {
        ProbePlatform::Windows
    }
    #[cfg(not(target_os = "windows"))]
    {
        ProbePlatform::Unix
    }
}

// ===================== bounded workflow subprocess runner =====================

/// Serialized workflow requests must stay within this protocol v1 bound.
pub const WORKFLOW_MAX_PROTOCOL_REQUEST_BYTES: usize = 1024 * 1024;
/// Stdout must contain exactly one JSON document bounded to this size.
pub const WORKFLOW_MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Stderr is diagnostic text only, bounded to this size.
pub const WORKFLOW_MAX_STDERR_BYTES: usize = 64 * 1024;
/// Script-reported warnings are capped at this count.
pub const WORKFLOW_MAX_WARNINGS: usize = 100;
/// Workflow subprocesses are bounded separately from image export workers.
pub const WORKFLOW_MAX_CONCURRENT_RUNS: usize = 4;
const WORKFLOW_RUN_POLL_INTERVAL: Duration = Duration::from_millis(10);
const WORKFLOW_PIPE_CHUNK_BYTES: usize = 8 * 1024;
const WORKFLOW_STDERR_EXCERPT_CHARS: usize = 400;
const WORKFLOW_STDERR_TRUNCATION_MARKER: &str = "\n...[stderr truncated]";

/// An interpreter resolved through probing, launched with an argument array
/// (`program prefix_args.. script`). Never a shell and never a concatenated
/// command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowInterpreter {
    pub language: WorkflowLanguage,
    pub program: String,
    pub prefix_args: Vec<String>,
}

fn workflow_interpreter(
    language: WorkflowLanguage,
    program: &str,
    prefix_args: &[&str],
) -> WorkflowInterpreter {
    WorkflowInterpreter {
        language,
        program: program.to_string(),
        prefix_args: prefix_args.iter().map(|arg| arg.to_string()).collect(),
    }
}

/// Execution candidates mirror the discovery probe order: the `py -3` launcher
/// first on Windows, `python3` first elsewhere, `node` for JavaScript.
pub fn workflow_interpreter_candidates(
    language: WorkflowLanguage,
    platform: ProbePlatform,
) -> Vec<WorkflowInterpreter> {
    match (language, platform) {
        (WorkflowLanguage::Python, ProbePlatform::Windows) => vec![
            workflow_interpreter(language, "py", &["-3"]),
            workflow_interpreter(language, "python", &[]),
        ],
        (WorkflowLanguage::Python, ProbePlatform::Unix) => vec![
            workflow_interpreter(language, "python3", &[]),
            workflow_interpreter(language, "python", &[]),
        ],
        (WorkflowLanguage::JavaScript, _) => vec![workflow_interpreter(language, "node", &[])],
    }
}

/// Resolves the first probeable interpreter for a language. Probing uses
/// argument arrays with a `--version` suffix, identical to discovery.
pub fn resolve_workflow_interpreter(
    language: WorkflowLanguage,
    platform: ProbePlatform,
    prober: &dyn RuntimeProber,
) -> Result<WorkflowInterpreter, WorkflowExecutionError> {
    let mut reasons = Vec::new();
    for interpreter in workflow_interpreter_candidates(language, platform) {
        let mut probe_args = interpreter.prefix_args.clone();
        probe_args.push("--version".to_string());
        let candidate = RuntimeCandidate {
            program: interpreter.program.clone(),
            args: probe_args,
        };
        match prober.probe(&candidate) {
            Ok(_) => return Ok(interpreter),
            Err(reason) => reasons.push(format!("'{}': {reason}", candidate.display())),
        }
    }
    Err(WorkflowExecutionError::MissingRuntime {
        language,
        reason: reasons.join("; "),
    })
}

/// Spawn-time revalidation of a workflow script's canonical identity.
///
/// Discovery records a canonical, symlink-free script path inside one of the
/// workflow roots. Between discovery and spawn the file may have been
/// substituted, so before any interpreter launches this function re-checks:
///
/// - the recorded path still exists and is a regular file, not a symlink or
///   other reparse point (component-wise substitution is rejected),
/// - it still canonicalizes to exactly the recorded canonical path, and
/// - the canonical path is still inside at least one canonical workflow root.
///
/// In-place content replacement of the same file is not detectable by path
/// identity and remains inside the trusted-local-code threat model. Returns
/// the validated canonical path to spawn.
pub fn validate_workflow_script_identity(
    recorded_canonical: &Path,
    canonical_roots: &[PathBuf],
) -> Result<PathBuf, WorkflowExecutionError> {
    let reject = |reason: String| Err(WorkflowExecutionError::ScriptIdentity { reason });
    let recorded = strip_verbatim(recorded_canonical);
    let metadata = std::fs::symlink_metadata(&recorded).map_err(|error| {
        WorkflowExecutionError::ScriptIdentity {
            reason: format!("'{}' is no longer readable: {error}", recorded.display()),
        }
    })?;
    if metadata.file_type().is_symlink() {
        return reject(format!(
            "'{}' became a symlink or reparse point after discovery",
            recorded.display()
        ));
    }
    if !metadata.file_type().is_file() {
        return reject(format!(
            "'{}' is no longer a regular file",
            recorded.display()
        ));
    }
    let canonical =
        recorded
            .canonicalize()
            .map_err(|error| WorkflowExecutionError::ScriptIdentity {
                reason: format!(
                    "'{}' could not be canonicalized: {error}",
                    recorded.display()
                ),
            })?;
    let canonical = strip_verbatim(&canonical);
    if canonical != recorded {
        return reject(format!(
            "'{}' now resolves to '{}' instead of the discovered path",
            recorded.display(),
            canonical.display()
        ));
    }
    let inside = canonical_roots
        .iter()
        .any(|root| path_is_within(&canonical, &strip_verbatim(root)));
    if !inside {
        return reject(format!(
            "'{}' canonicalizes outside every workflow root",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// Resolves a bare program name against one `PATH` value, mirroring the
/// platform search: every directory in order, first regular file wins, with
/// `PATHEXT` extensions on Windows and the executable bit on Unix. Symlinks
/// are followed (system interpreters are commonly symlinked); the caller
/// canonicalizes the result to pin the final identity.
pub fn resolve_program_on_path(
    program: &str,
    path_value: &str,
    pathext_value: Option<&str>,
) -> Option<PathBuf> {
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return None;
    }
    let extensions: Vec<String> = if cfg!(windows) {
        pathext_value
            .filter(|value| !value.is_empty())
            .unwrap_or(".EXE;.CMD;.BAT;.COM")
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| {
                if ext.starts_with('.') {
                    ext.to_string()
                } else {
                    format!(".{ext}")
                }
            })
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(path_value) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for extension in &extensions {
            let candidate = dir.join(format!("{program}{extension}"));
            // Follow symlinks: the target's file type and permissions decide.
            let Ok(metadata) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
}

/// Validates and pins the interpreter executable for one spawn: bare names
/// resolve through `PATH` (with `PATHEXT` on Windows), explicit paths are
/// used as given. The returned path is canonical, exists, and is a regular
/// file, so the process launched is exactly the one validated — a probe-time
/// program that was deleted or replaced by a non-file never spawns.
fn resolve_interpreter_executable(program: &str) -> Result<PathBuf, WorkflowExecutionError> {
    let direct = PathBuf::from(program);
    let resolved = if program.contains('/') || program.contains('\\') {
        direct
    } else {
        std::env::var("PATH")
            .ok()
            .and_then(|path| {
                let pathext = std::env::var("PATHEXT").ok();
                resolve_program_on_path(program, &path, pathext.as_deref())
            })
            .unwrap_or(direct)
    };
    let canonical = resolved
        .canonicalize()
        .map_err(|error| WorkflowExecutionError::Spawn {
            reason: format!(
                "interpreter '{program}' could not be resolved to an executable file: {error}"
            ),
        })?;
    let canonical = strip_verbatim(&canonical);
    if !canonical.is_file() {
        return Err(WorkflowExecutionError::Spawn {
            reason: format!(
                "interpreter '{program}' resolved to '{}' which is not a regular file",
                canonical.display()
            ),
        });
    }
    Ok(canonical)
}

/// Finite bounds for one invocation; every field is enforced by the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowRunLimits {
    pub timeout: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl Default for WorkflowRunLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(WORKFLOW_DEFAULT_TIMEOUT_SECONDS),
            max_stdout_bytes: WORKFLOW_MAX_RESPONSE_BYTES,
            max_stderr_bytes: WORKFLOW_MAX_STDERR_BYTES,
        }
    }
}

/// One bounded workflow invocation: interpreter, canonical script, serialized
/// request, controlled working directory, limits, and the roots inside which
/// produced artifacts count as internal.
#[derive(Debug)]
pub struct WorkflowRunSpec<'a> {
    pub interpreter: &'a WorkflowInterpreter,
    pub script_path: &'a Path,
    pub request_json: String,
    pub working_directory: &'a Path,
    pub limits: WorkflowRunLimits,
    pub artifact_roots: &'a [PathBuf],
}

impl<'a> WorkflowRunSpec<'a> {
    /// Serializes the request and rejects anything over the 1 MiB protocol
    /// bound before a child is ever spawned.
    pub fn new(
        interpreter: &'a WorkflowInterpreter,
        script_path: &'a Path,
        request: &WorkflowRequest,
        working_directory: &'a Path,
        artifact_roots: &'a [PathBuf],
    ) -> Result<Self, String> {
        let request_json = serde_json::to_string(request)
            .map_err(|error| format!("workflow request could not be serialized: {error}"))?;
        if request_json.len() > WORKFLOW_MAX_PROTOCOL_REQUEST_BYTES {
            return Err(format!(
                "serialized workflow request is {} bytes, over the {} byte protocol bound",
                request_json.len(),
                WORKFLOW_MAX_PROTOCOL_REQUEST_BYTES
            ));
        }
        Ok(Self {
            interpreter,
            script_path,
            request_json,
            working_directory,
            limits: WorkflowRunLimits::default(),
            artifact_roots,
        })
    }

    pub fn with_limits(mut self, limits: WorkflowRunLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Successful invocation payload: the parsed response (warnings capped),
/// produced artifacts canonicalized and labeled, plus bounded stderr text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunOutput {
    pub response: WorkflowResponse,
    pub produced_artifacts: Vec<WorkflowArtifact>,
    pub stderr: String,
}

/// Injected process boundary for one workflow invocation. Implementations must
/// never use a shell and must terminate the whole child tree on cancellation,
/// timeout, or output-limit violations.
pub trait WorkflowRunner: Send + Sync {
    fn run(
        &self,
        spec: &WorkflowRunSpec<'_>,
        cancel: &AtomicBool,
    ) -> Result<WorkflowRunOutput, WorkflowExecutionError>;
}

/// Bounds simultaneous workflow subprocesses independently of export workers.
#[derive(Debug, Clone)]
pub struct WorkflowConcurrencyGate {
    semaphore: Arc<tokio::sync::Semaphore>,
    limit: usize,
}

impl WorkflowConcurrencyGate {
    pub fn new(limit: usize) -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(limit)),
            limit,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn try_acquire(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.semaphore).try_acquire_owned().ok()
    }
}

impl Default for WorkflowConcurrencyGate {
    fn default() -> Self {
        Self::new(WORKFLOW_MAX_CONCURRENT_RUNS)
    }
}

/// Minimal documented child environment: `PATH`, `TEMP`/`TMP`,
/// `HOME`/`USERPROFILE`, plus `SYSTEMROOT`/`COMSPEC` on Windows. The parent
/// environment is never forwarded wholesale, so arbitrary secrets stay
/// private. Values are never logged.
fn minimal_workflow_environment() -> Vec<(String, String)> {
    #[cfg(windows)]
    const ALLOWLIST: [&str; 6] = [
        "PATH",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "SYSTEMROOT",
        "COMSPEC",
    ];
    #[cfg(not(windows))]
    const ALLOWLIST: [&str; 4] = ["PATH", "TEMP", "TMP", "HOME"];
    ALLOWLIST
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_string(), value))
        })
        .collect()
}

#[cfg(windows)]
mod windows_job {
    use std::process::Child;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// Kill-on-close job object so every descendant dies with the workflow
    /// invocation, even when the direct child has already exited and even if
    /// the host crashes. `taskkill /T` cannot help once the leader is gone, so
    /// the job object is the only reliable tree-wide termination on Windows.
    pub struct WorkflowJob {
        handle: HANDLE,
    }

    impl WorkflowJob {
        pub fn create_kill_on_close() -> Option<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0 {
                    windows_sys::Win32::Foundation::CloseHandle(handle);
                    return None;
                }
                Some(Self { handle })
            }
        }

        pub fn assign(&self, child: &Child) -> bool {
            use std::os::windows::io::AsRawHandle;
            unsafe { AssignProcessToJobObject(self.handle, child.as_raw_handle() as HANDLE) != 0 }
        }

        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.handle, 1);
            }
        }
    }

    impl Drop for WorkflowJob {
        fn drop(&mut self) {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

/// Owns tree-wide termination for one child: a job object on Windows, a
/// dedicated process group (`setpgid(0)`) killed with `kill(-pgid, SIGKILL)`
/// on Unix. Falls back to a direct child kill only if job creation failed.
struct ChildTreeGuard {
    #[cfg(windows)]
    job: Option<windows_job::WorkflowJob>,
    #[cfg(unix)]
    pid: u32,
}

impl ChildTreeGuard {
    #[cfg(windows)]
    fn new() -> Self {
        Self {
            job: windows_job::WorkflowJob::create_kill_on_close(),
        }
    }

    #[cfg(unix)]
    fn new(pid: u32) -> Self {
        Self { pid }
    }

    fn adopt(&self, child: &std::process::Child) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.assign(child);
        }
        #[cfg(unix)]
        let _ = child;
    }

    fn terminate(&self, child: &mut std::process::Child) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(not(windows))]
        {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// One captured child stream, bounded to its byte limit.
struct BoundedPipe {
    bytes: Mutex<Vec<u8>>,
    exceeded: AtomicBool,
    finished: AtomicBool,
}

impl BoundedPipe {
    fn new() -> Self {
        Self {
            bytes: Mutex::new(Vec::new()),
            exceeded: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        }
    }
}

fn spawn_bounded_reader(
    mut reader: impl std::io::Read + Send + 'static,
    limit: usize,
    pipe: Arc<BoundedPipe>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; WORKFLOW_PIPE_CHUNK_BYTES];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    let mut bytes = pipe.bytes.lock().unwrap();
                    if bytes.len() + read > limit {
                        pipe.exceeded.store(true, Ordering::Relaxed);
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..read]);
                }
                Err(_) => break,
            }
        }
        pipe.finished.store(true, Ordering::Relaxed);
    })
}

fn stderr_display_text(bytes: &[u8], exceeded: bool) -> String {
    let mut text = String::from_utf8_lossy(bytes).to_string();
    if exceeded {
        text.push_str(WORKFLOW_STDERR_TRUNCATION_MARKER);
    }
    text
}

/// Suffix marking a bounded diagnostic excerpt that was cut short.
pub const WORKFLOW_STDERR_EXCERPT_TRUNCATION_MARKER: &str = "...[truncated]";

/// Bounds a diagnostic text to [`WORKFLOW_STDERR_EXCERPT_CHARS`] characters and
/// redacts the given home directory (both slash variants) to `~`. Excerpts are
/// the only workflow subprocess text ever forwarded to the frontend.
pub fn redact_workflow_text(text: &str, home: Option<&str>) -> String {
    let mut redacted = text.to_string();
    if let Some(home) = home.filter(|home| !home.is_empty()) {
        for variant in [home.to_string(), home.replace('\\', "/")] {
            if !variant.is_empty() {
                redacted = redacted.replace(&variant, "~");
            }
        }
    }
    let total = redacted.chars().count();
    let mut bounded: String = redacted
        .chars()
        .take(WORKFLOW_STDERR_EXCERPT_CHARS)
        .collect();
    if total > WORKFLOW_STDERR_EXCERPT_CHARS {
        bounded.push_str(WORKFLOW_STDERR_EXCERPT_TRUNCATION_MARKER);
    }
    bounded
}

/// [`redact_workflow_text`] with the current user's home directory.
pub fn redact_workflow_stderr(text: &str) -> String {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").ok();
    #[cfg(not(windows))]
    let home = std::env::var("HOME").ok();
    redact_workflow_text(text, home.as_deref())
}

fn malformed_output_reason(error: serde_json::Error, stderr_text: &str) -> String {
    let trimmed = stderr_text.trim();
    let base = format!("stdout was not a valid protocol v1 JSON document: {error}");
    if trimmed.is_empty() {
        base
    } else {
        let excerpt: String = trimmed
            .chars()
            .take(WORKFLOW_STDERR_EXCERPT_CHARS)
            .collect();
        format!("{base}; stderr: {excerpt}")
    }
}

/// Canonicalizes produced artifact paths. Relative paths resolve against the
/// working directory; anything outside every root (including unresolvable
/// paths) is labeled `external` so the UI can treat it accordingly.
pub fn canonicalize_workflow_artifacts(
    paths: &[String],
    working_directory: &Path,
    artifact_roots: &[PathBuf],
) -> Vec<WorkflowArtifact> {
    let mut roots: Vec<PathBuf> = Vec::with_capacity(artifact_roots.len() + 1);
    roots.push(working_directory.to_path_buf());
    roots.extend_from_slice(artifact_roots);
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok().map(|root| strip_verbatim(&root)))
        .collect();
    paths
        .iter()
        .map(|path| {
            let candidate = Path::new(path);
            let joined = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                working_directory.join(candidate)
            };
            match joined.canonicalize() {
                Ok(canonical) => {
                    let canonical = strip_verbatim(&canonical);
                    let inside = canonical_roots
                        .iter()
                        .any(|root| path_is_within(&canonical, root));
                    WorkflowArtifact {
                        path: canonical.to_string_lossy().to_string(),
                        kind: (!inside).then(|| "external".to_string()),
                    }
                }
                Err(_) => WorkflowArtifact {
                    path: strip_verbatim(&joined).to_string_lossy().to_string(),
                    kind: Some("external".to_string()),
                },
            }
        })
        .collect()
}

enum RunTermination {
    Completed,
    Cancelled,
    TimedOut,
    Exceeded(WorkflowOutputStream),
}

/// Default injected runner: spawns the interpreter directly with an argument
/// array, a cleared-and-allowlisted environment, the workspace temp directory
/// as CWD, pipes one request through stdin, and enforces finite timeout and
/// byte limits while polling a cancellation flag.
#[derive(Debug, Clone, Default)]
pub struct CommandWorkflowRunner;

impl WorkflowRunner for CommandWorkflowRunner {
    fn run(
        &self,
        spec: &WorkflowRunSpec<'_>,
        cancel: &AtomicBool,
    ) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
        run_workflow_process(spec, cancel)
    }
}

fn run_workflow_process(
    spec: &WorkflowRunSpec<'_>,
    cancel: &AtomicBool,
) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
    // Pin the interpreter identity first: bare names resolve through PATH,
    // the resolved file must exist and be a regular file, and the canonical
    // path is what gets spawned — never an unvalidated name lookup.
    let interpreter_executable = resolve_interpreter_executable(&spec.interpreter.program)?;
    let mut command = Command::new(&interpreter_executable);
    command
        .args(&spec.interpreter.prefix_args)
        .arg(spec.script_path)
        .current_dir(spec.working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(minimal_workflow_environment());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Dedicated process group so kill(-pgid) reaches every descendant.
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|error| WorkflowExecutionError::Spawn {
            reason: error.to_string(),
        })?;
    #[cfg(windows)]
    let guard = ChildTreeGuard::new();
    #[cfg(unix)]
    let guard = ChildTreeGuard::new(child.id());
    guard.adopt(&child);

    let mut stdin = child.stdin.take().expect("piped stdin");
    let request_bytes = spec.request_json.clone().into_bytes();
    let stdin_writer = std::thread::spawn(move || {
        use std::io::Write;
        // A broken pipe simply means the child exited without reading; the
        // child's exit status and output decide the outcome.
        let _ = stdin.write_all(&request_bytes);
    });

    let stdout_pipe = Arc::new(BoundedPipe::new());
    let stderr_pipe = Arc::new(BoundedPipe::new());
    let stdout_reader = spawn_bounded_reader(
        child.stdout.take().expect("piped stdout"),
        spec.limits.max_stdout_bytes,
        Arc::clone(&stdout_pipe),
    );
    let stderr_reader = spawn_bounded_reader(
        child.stderr.take().expect("piped stderr"),
        spec.limits.max_stderr_bytes,
        Arc::clone(&stderr_pipe),
    );

    let deadline = Instant::now() + spec.limits.timeout;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    let termination = loop {
        if stdout_pipe.exceeded.load(Ordering::Relaxed) {
            break RunTermination::Exceeded(WorkflowOutputStream::Stdout);
        }
        if stderr_pipe.exceeded.load(Ordering::Relaxed) {
            break RunTermination::Exceeded(WorkflowOutputStream::Stderr);
        }
        if cancel.load(Ordering::Relaxed) {
            break RunTermination::Cancelled;
        }
        if exit_status.is_none() {
            match child.try_wait() {
                Ok(status) => exit_status = status,
                Err(error) => {
                    guard.terminate(&mut child);
                    return Err(WorkflowExecutionError::Spawn {
                        reason: format!("workflow process could not be polled: {error}"),
                    });
                }
            }
        }
        // The run is only complete when the child exited AND both streams
        // settled; a grandchild can keep a pipe open after its leader died.
        if exit_status.is_some()
            && stdout_pipe.finished.load(Ordering::Relaxed)
            && stderr_pipe.finished.load(Ordering::Relaxed)
        {
            break RunTermination::Completed;
        }
        if Instant::now() >= deadline {
            break RunTermination::TimedOut;
        }
        std::thread::sleep(WORKFLOW_RUN_POLL_INTERVAL);
    };

    if matches!(termination, RunTermination::Completed) {
        let _ = child.wait();
    } else {
        // Tree-wide kill first: descendants hold pipe write ends, so readers
        // and the stdin writer only finish after the whole tree is gone.
        guard.terminate(&mut child);
    }
    let _ = stdin_writer.join();
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();

    let stdout_bytes = stdout_pipe.bytes.lock().unwrap().clone();
    let stderr_bytes = stderr_pipe.bytes.lock().unwrap().clone();
    let stderr_exceeded = stderr_pipe.exceeded.load(Ordering::Relaxed);
    let stderr_text = stderr_display_text(&stderr_bytes, stderr_exceeded);

    match termination {
        RunTermination::Exceeded(stream) => {
            Err(WorkflowExecutionError::OutputLimitExceeded { stream })
        }
        RunTermination::Cancelled => Err(WorkflowExecutionError::Cancelled),
        RunTermination::TimedOut => Err(WorkflowExecutionError::TimedOut),
        RunTermination::Completed => {
            let status = exit_status.expect("completed termination implies an exit status");
            if !status.success() {
                return Err(WorkflowExecutionError::NonZeroExit {
                    code: status.code().unwrap_or(-1),
                });
            }
            let stdout_text = String::from_utf8_lossy(&stdout_bytes);
            let response: WorkflowResponse =
                serde_json::from_str(&stdout_text).map_err(|error| {
                    WorkflowExecutionError::MalformedOutput {
                        reason: malformed_output_reason(error, &stderr_text),
                    }
                })?;
            Ok(finalize_workflow_response(response, spec, stderr_text))
        }
    }
}

fn finalize_workflow_response(
    mut response: WorkflowResponse,
    spec: &WorkflowRunSpec<'_>,
    stderr_text: String,
) -> WorkflowRunOutput {
    response.warnings.truncate(WORKFLOW_MAX_WARNINGS);
    let produced_artifacts = canonicalize_workflow_artifacts(
        &response.produced_artifact_paths,
        spec.working_directory,
        spec.artifact_roots,
    );
    WorkflowRunOutput {
        response,
        produced_artifacts,
        stderr: stderr_text,
    }
}

// ===================== export pipeline integration =====================

/// Typed reasons a selected workflow list cannot be planned for an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowSelectionError {
    UnknownWorkflow { id: String },
    UnselectableWorkflow { id: String },
    UnavailableRuntime { id: String, reason: String },
    DuplicateSelection { id: String },
}

impl std::fmt::Display for WorkflowSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownWorkflow { id } => {
                write!(f, "workflow '{id}' is not in the workflow registry")
            }
            Self::UnselectableWorkflow { id } => write!(
                f,
                "workflow '{id}' has invalid metadata and cannot be selected"
            ),
            Self::UnavailableRuntime { id, reason } => {
                write!(f, "workflow '{id}' has no available runtime: {reason}")
            }
            Self::DuplicateSelection { id } => {
                write!(f, "workflow '{id}' was selected more than once")
            }
        }
    }
}

impl std::error::Error for WorkflowSelectionError {}

/// Unique run id shared by every workflow invocation of one export task.
pub fn generate_workflow_run_id() -> String {
    static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!(
        "run-{:x}-{}",
        nanos,
        RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// An immutable snapshot of the selected registry entries for one export:
/// the entries are cloned from a discovery result at export start, so later
/// rescans cannot change a running export. Selection order (not registry
/// order) defines execution order within each phase.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportWorkflowPlan {
    pub run_id: String,
    pub post_image: Vec<DiscoveredWorkflow>,
    pub post_batch: Vec<DiscoveredWorkflow>,
}

/// Resolves selected workflow ids against a discovery snapshot, preserving
/// the user-selected order. `Ok(None)` means no workflows were selected and
/// the export must follow the plain no-workflow path.
pub fn plan_export_workflows(
    selected_ids: &[String],
    discovery: &WorkflowDiscoveryResult,
) -> Result<Option<ExportWorkflowPlan>, WorkflowSelectionError> {
    plan_export_workflows_with_run_id(generate_workflow_run_id(), selected_ids, discovery)
}

/// [`plan_export_workflows`] with a caller-supplied run id, so a progress sink
/// can report the `Discovering` phase before resolution starts.
pub fn plan_export_workflows_with_run_id(
    run_id: String,
    selected_ids: &[String],
    discovery: &WorkflowDiscoveryResult,
) -> Result<Option<ExportWorkflowPlan>, WorkflowSelectionError> {
    if selected_ids.is_empty() {
        return Ok(None);
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut post_image = Vec::new();
    let mut post_batch = Vec::new();
    for id in selected_ids {
        if !seen.insert(id.clone()) {
            return Err(WorkflowSelectionError::DuplicateSelection { id: id.clone() });
        }
        let workflow = discovery
            .workflows
            .iter()
            .find(|workflow| &workflow.id == id)
            .ok_or_else(|| WorkflowSelectionError::UnknownWorkflow { id: id.clone() })?;
        if !workflow.selectable {
            return Err(WorkflowSelectionError::UnselectableWorkflow { id: id.clone() });
        }
        if !workflow.runtime.available {
            return Err(WorkflowSelectionError::UnavailableRuntime {
                id: id.clone(),
                reason: workflow
                    .runtime
                    .unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "unknown reason".to_string()),
            });
        }
        match workflow.phase {
            WorkflowPhase::PostImage => post_image.push(workflow.clone()),
            WorkflowPhase::PostBatch => post_batch.push(workflow.clone()),
        }
    }
    Ok(Some(ExportWorkflowPlan {
        run_id,
        post_image,
        post_batch,
    }))
}

/// A policy-governed failure carrying the typed reason the affected image or
/// batch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowPolicyFailure {
    ScriptReported {
        workflow_id: String,
        message: String,
    },
    ExecutionError {
        workflow_id: String,
        error: WorkflowExecutionError,
    },
}

impl WorkflowPolicyFailure {
    fn status(&self) -> WorkflowRunStatus {
        match self {
            Self::ScriptReported { .. } => WorkflowRunStatus::Failed,
            Self::ExecutionError { error, .. } => error.status(),
        }
    }
}

impl std::fmt::Display for WorkflowPolicyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScriptReported {
                workflow_id,
                message,
            } => {
                write!(f, "workflow '{workflow_id}' reported failure: {message}")
            }
            Self::ExecutionError { workflow_id, error } => {
                write!(f, "workflow '{workflow_id}' failed: {error}")
            }
        }
    }
}

impl std::error::Error for WorkflowPolicyFailure {}

/// Terminal outcome of one workflow phase for an image or the whole batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowPhaseError {
    /// An `onError=fail` workflow failed; the reason is typed.
    Failed(WorkflowPolicyFailure),
    /// The export was cancelled before or during the phase.
    Cancelled,
    /// Host-side setup failed before any child process was spawned.
    Setup(String),
}

impl std::fmt::Display for WorkflowPhaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(failure) => write!(f, "export workflow failed: {failure}"),
            Self::Cancelled => write!(f, "Export cancelled"),
            Self::Setup(reason) => write!(f, "export workflow setup failed: {reason}"),
        }
    }
}

impl std::error::Error for WorkflowPhaseError {}

/// Applies a workflow's `onError` policy to one failure. `warn` records the
/// failure on the returned run result and lets the export continue; `fail`
/// fails the affected image or batch with the typed reason.
fn apply_workflow_error_policy(
    workflow: &DiscoveredWorkflow,
    source_path: Option<String>,
    failure: WorkflowPolicyFailure,
) -> Result<WorkflowRunResult, WorkflowPhaseError> {
    match workflow.on_error {
        WorkflowErrorPolicy::Warn => {
            log::warn!(
                "Export workflow '{}' degraded (onError=warn): {}",
                workflow.id,
                failure
            );
            Ok(WorkflowRunResult {
                workflow_id: workflow.id.clone(),
                source_path,
                status: failure.status(),
                message: match &failure {
                    WorkflowPolicyFailure::ScriptReported { message, .. } => Some(message.clone()),
                    WorkflowPolicyFailure::ExecutionError { error, .. } => Some(error.to_string()),
                },
                warnings: Vec::new(),
                produced_artifacts: Vec::new(),
                stderr_excerpt: None,
            })
        }
        WorkflowErrorPolicy::Fail => {
            log::error!(
                "Export workflow '{}' failed (onError=fail): {}",
                workflow.id,
                failure
            );
            Err(WorkflowPhaseError::Failed(failure))
        }
    }
}

/// The workflow integration service shared by GUI and headless exports.
///
/// One engine instance serves one export task: it sequences the selected
/// postImage workflows per image in user-selected order after that image's
/// outputs are fully written, invokes the selected postBatch workflows once
/// after all image workers settle, enforces each workflow's `onError` policy,
/// bounds concurrent workflow subprocesses through the shared
/// [`WorkflowConcurrencyGate`] independently of export render workers, and
/// owns the per-run workspace temp directory lifecycle.
pub struct ExportWorkflowEngine {
    plan: ExportWorkflowPlan,
    runner: Arc<dyn WorkflowRunner>,
    prober: Arc<dyn RuntimeProber>,
    gate: WorkflowConcurrencyGate,
    export_settings: WorkflowExportSettings,
    workspace_parent: PathBuf,
    workspace: Mutex<Option<Arc<PathBuf>>>,
    interpreters: Mutex<HashMap<WorkflowLanguage, Arc<WorkflowInterpreter>>>,
    /// Canonical workflow roots (bundled and user). Every invocation
    /// revalidates the script's canonical identity against these before any
    /// interpreter spawns; an empty list rejects every script (fail closed).
    workflow_roots: Vec<PathBuf>,
    total: u64,
    progress: Option<WorkflowProgressSink>,
    warnings_seen: AtomicU64,
}

impl ExportWorkflowEngine {
    /// Builds the engine for one export run. `workspace_parent` is the
    /// directory under which the per-run workspace `<run_id>` is created
    /// lazily on the first workflow invocation. `workflow_roots` holds the
    /// canonical discovery roots used for spawn-time script identity
    /// revalidation. `progress`, when present, receives one typed event per
    /// lifecycle transition.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan: ExportWorkflowPlan,
        runner: Arc<dyn WorkflowRunner>,
        prober: Arc<dyn RuntimeProber>,
        gate: WorkflowConcurrencyGate,
        export_settings: WorkflowExportSettings,
        workspace_parent: PathBuf,
        workflow_roots: Vec<PathBuf>,
        total: u64,
        progress: Option<WorkflowProgressSink>,
    ) -> Self {
        Self {
            plan,
            runner,
            prober,
            gate,
            export_settings,
            workspace_parent,
            workspace: Mutex::new(None),
            interpreters: Mutex::new(HashMap::new()),
            workflow_roots,
            total,
            progress,
            warnings_seen: AtomicU64::new(0),
        }
    }

    pub fn run_id(&self) -> &str {
        &self.plan.run_id
    }

    /// Warnings and warn-policy degradations counted so far in this run.
    pub fn warning_count(&self) -> u64 {
        self.warnings_seen.load(Ordering::Relaxed)
    }

    pub fn has_post_image(&self) -> bool {
        !self.plan.post_image.is_empty()
    }

    pub fn has_post_batch(&self) -> bool {
        !self.plan.post_batch.is_empty()
    }

    /// Forwards one event to the sink, if any. Never blocks the pipeline.
    fn emit(&self, mut event: WorkflowProgressEvent) {
        if let Some(sink) = &self.progress {
            if event.phase != WorkflowProgressPhase::Waiting {
                event.warning_count = self.warnings_seen.load(Ordering::Relaxed);
            }
            sink(event);
        }
    }

    /// Base event for one invocation of a workflow.
    fn invocation_event(
        &self,
        phase: WorkflowProgressPhase,
        workflow: &DiscoveredWorkflow,
        source_path: Option<&str>,
        index: u64,
    ) -> WorkflowProgressEvent {
        WorkflowProgressEvent {
            run_id: self.plan.run_id.clone(),
            phase,
            workflow_id: Some(workflow.id.clone()),
            source_path: source_path.map(str::to_string),
            index,
            total: self.total,
            timeout_seconds: Some(workflow.timeout_seconds),
            warning_count: self.warnings_seen.load(Ordering::Relaxed),
        }
    }

    /// Counts warnings and warn-policy degradations of a settled run so later
    /// events carry an honest running total.
    fn note_warnings(&self, result: &WorkflowRunResult) {
        let degradations = u64::from(matches!(
            result.status,
            WorkflowRunStatus::Failed | WorkflowRunStatus::TimedOut
        ));
        self.warnings_seen.fetch_add(
            result.warnings.len() as u64 + degradations,
            Ordering::Relaxed,
        );
    }

    /// Runs the selected postImage workflows for one image, sequentially in
    /// user-selected order so they never race on the same output. Must be
    /// called only after the image's file, metadata, timestamps, and mask
    /// artifacts are fully written. Produced artifacts are chained: each
    /// workflow receives the artifacts of its predecessors plus its own
    /// output in `artifacts`, and the caller's list is extended so downstream
    /// records carry them.
    pub fn run_post_image(
        &self,
        cancel: &AtomicBool,
        source_path: &str,
        exported_path: &str,
        artifacts: &mut Vec<WorkflowArtifact>,
        index: u64,
        export_root: &Path,
    ) -> Result<Vec<WorkflowRunResult>, WorkflowPhaseError> {
        let mut results = Vec::with_capacity(self.plan.post_image.len());
        for workflow in &self.plan.post_image {
            if cancel.load(Ordering::SeqCst) {
                return Err(WorkflowPhaseError::Cancelled);
            }
            let workspace = self.workspace()?;
            let base = self.invocation_event(
                WorkflowProgressPhase::RunningPostImage,
                workflow,
                Some(source_path),
                index,
            );
            self.emit(base.clone());
            let request = WorkflowRequest {
                protocol_version: WORKFLOW_PROTOCOL_VERSION,
                run_id: self.plan.run_id.clone(),
                workflow_id: workflow.id.clone(),
                phase: WorkflowPhase::PostImage,
                source_path: Some(source_path.to_string()),
                exported_path: Some(exported_path.to_string()),
                artifacts: artifacts.clone(),
                selected_items: Vec::new(),
                exported_items: Vec::new(),
                export_settings: self.export_settings.clone(),
                index: Some(index),
                total: self.total,
                workspace_temp_directory: workspace.to_string_lossy().to_string(),
            };
            let result = self.invoke(workflow, request, cancel, export_root, &base)?;
            artifacts.extend(result.produced_artifacts.iter().cloned());
            results.push(result);
        }
        Ok(results)
    }

    /// Invokes the selected postBatch workflows exactly once for the export,
    /// sequentially in user-selected order, with the settled item records:
    /// every selected item plus every exported item, including failed and
    /// cancelled-skipped entries. Must be called only after all image workers
    /// have settled. Skips every workflow when the export was cancelled.
    pub fn run_post_batch(
        &self,
        cancel: &AtomicBool,
        selected_items: &[WorkflowItem],
        exported_items: &[WorkflowItem],
        export_root: &Path,
    ) -> Result<Vec<WorkflowRunResult>, WorkflowPhaseError> {
        if cancel.load(Ordering::SeqCst) {
            return Err(WorkflowPhaseError::Cancelled);
        }
        let mut results = Vec::with_capacity(self.plan.post_batch.len());
        for workflow in &self.plan.post_batch {
            if cancel.load(Ordering::SeqCst) {
                return Err(WorkflowPhaseError::Cancelled);
            }
            let workspace = self.workspace()?;
            let base = self.invocation_event(
                WorkflowProgressPhase::RunningPostBatch,
                workflow,
                None,
                self.total,
            );
            self.emit(base.clone());
            let request = WorkflowRequest {
                protocol_version: WORKFLOW_PROTOCOL_VERSION,
                run_id: self.plan.run_id.clone(),
                workflow_id: workflow.id.clone(),
                phase: WorkflowPhase::PostBatch,
                source_path: None,
                exported_path: None,
                artifacts: Vec::new(),
                selected_items: selected_items.to_vec(),
                exported_items: exported_items.to_vec(),
                export_settings: self.export_settings.clone(),
                index: None,
                total: self.total,
                workspace_temp_directory: workspace.to_string_lossy().to_string(),
            };
            let result = self.invoke(workflow, request, cancel, export_root, &base)?;
            results.push(result);
        }
        Ok(results)
    }

    /// Removes the per-run workspace directory. Best effort: a failed removal
    /// is logged and never fails the export.
    pub fn cleanup(&self) {
        if let Some(workspace) = self.workspace.lock().unwrap().take()
            && let Err(error) = std::fs::remove_dir_all(workspace.as_ref())
        {
            log::warn!(
                "Workflow workspace '{}' could not be removed: {error}",
                workspace.display()
            );
        }
    }

    /// Waits for a workflow subprocess slot on the shared gate, independent
    /// of export render worker permits. Emits `Waiting` once when the gate is
    /// contended. Returns `Cancelled` as soon as the export is cancelled so
    /// unstarted workflows are skipped.
    fn acquire_gate(
        &self,
        cancel: &AtomicBool,
        wait_event: &WorkflowProgressEvent,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, WorkflowPhaseError> {
        let mut announced = false;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(WorkflowPhaseError::Cancelled);
            }
            if let Some(permit) = self.gate.try_acquire() {
                return Ok(permit);
            }
            if !announced {
                announced = true;
                let mut event = wait_event.clone();
                event.phase = WorkflowProgressPhase::Waiting;
                self.emit(event);
            }
            std::thread::sleep(WORKFLOW_RUN_POLL_INTERVAL);
        }
    }

    /// Resolves (once per language per run) and caches the interpreter.
    fn interpreter_for(
        &self,
        language: WorkflowLanguage,
    ) -> Result<Arc<WorkflowInterpreter>, WorkflowExecutionError> {
        let mut cache = self.interpreters.lock().unwrap();
        if let Some(interpreter) = cache.get(&language) {
            return Ok(Arc::clone(interpreter));
        }
        let resolved = Arc::new(resolve_workflow_interpreter(
            language,
            discovery_probe_platform(),
            self.prober.as_ref(),
        )?);
        cache.insert(language, Arc::clone(&resolved));
        Ok(resolved)
    }

    /// The per-run workspace directory, created lazily on first use.
    fn workspace(&self) -> Result<Arc<PathBuf>, WorkflowPhaseError> {
        let mut slot = self.workspace.lock().unwrap();
        if let Some(existing) = slot.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let workspace = self.workspace_parent.join(&self.plan.run_id);
        std::fs::create_dir_all(&workspace).map_err(|error| {
            WorkflowPhaseError::Setup(format!(
                "workflow workspace '{}' could not be created: {error}",
                workspace.display()
            ))
        })?;
        let workspace = Arc::new(workspace);
        *slot = Some(Arc::clone(&workspace));
        Ok(workspace)
    }

    /// Runs one bounded invocation through the gate and interprets the
    /// outcome through the workflow's `onError` policy. Cancellation always
    /// propagates as `Cancelled`, never as a policy failure. Warn-policy
    /// degradations feed the running warning counter instead of failing the
    /// item.
    fn invoke(
        &self,
        workflow: &DiscoveredWorkflow,
        request: WorkflowRequest,
        cancel: &AtomicBool,
        export_root: &Path,
        base_event: &WorkflowProgressEvent,
    ) -> Result<WorkflowRunResult, WorkflowPhaseError> {
        let _permit = self.acquire_gate(cancel, base_event)?;
        let interpreter = match self.interpreter_for(workflow.language) {
            Ok(interpreter) => interpreter,
            Err(error) => {
                let outcome = apply_workflow_error_policy(
                    workflow,
                    request.source_path.clone(),
                    WorkflowPolicyFailure::ExecutionError {
                        workflow_id: workflow.id.clone(),
                        error,
                    },
                );
                if let Ok(degraded) = &outcome {
                    self.note_warnings(degraded);
                }
                return outcome;
            }
        };
        let workspace = self.workspace()?;
        let script_path = PathBuf::from(&workflow.script_path);
        // Revalidate the script's canonical identity before building the
        // spec: discovery's snapshot must still match the file on disk and
        // stay inside a workflow root, or the invocation is refused.
        if let Err(error) = validate_workflow_script_identity(&script_path, &self.workflow_roots) {
            let outcome = apply_workflow_error_policy(
                workflow,
                request.source_path.clone(),
                WorkflowPolicyFailure::ExecutionError {
                    workflow_id: workflow.id.clone(),
                    error,
                },
            );
            if let Ok(degraded) = &outcome {
                self.note_warnings(degraded);
            }
            return outcome;
        }
        let artifact_roots = vec![workspace.as_ref().to_path_buf(), export_root.to_path_buf()];
        let spec = WorkflowRunSpec::new(
            interpreter.as_ref(),
            &script_path,
            &request,
            workspace.as_ref(),
            &artifact_roots,
        )
        .map_err(WorkflowPhaseError::Setup)?
        .with_limits(WorkflowRunLimits {
            timeout: Duration::from_secs(workflow.timeout_seconds),
            ..WorkflowRunLimits::default()
        });

        match self.runner.run(&spec, cancel) {
            Err(WorkflowExecutionError::Cancelled) => Err(WorkflowPhaseError::Cancelled),
            Err(error) => {
                let outcome = apply_workflow_error_policy(
                    workflow,
                    request.source_path.clone(),
                    WorkflowPolicyFailure::ExecutionError {
                        workflow_id: workflow.id.clone(),
                        error,
                    },
                );
                if let Ok(degraded) = &outcome {
                    self.note_warnings(degraded);
                }
                outcome
            }
            Ok(output) => {
                let stderr_excerpt = (!output.stderr.trim().is_empty())
                    .then(|| redact_workflow_stderr(&output.stderr));
                if output.response.ok {
                    let status = if output.response.warnings.is_empty() {
                        WorkflowRunStatus::Succeeded
                    } else {
                        WorkflowRunStatus::Warned
                    };
                    let result = WorkflowRunResult {
                        workflow_id: workflow.id.clone(),
                        source_path: request.source_path.clone(),
                        status,
                        message: output.response.message.clone(),
                        warnings: output.response.warnings.clone(),
                        produced_artifacts: output.produced_artifacts.clone(),
                        stderr_excerpt,
                    };
                    self.note_warnings(&result);
                    Ok(result)
                } else {
                    let outcome = apply_workflow_error_policy(
                        workflow,
                        request.source_path.clone(),
                        WorkflowPolicyFailure::ScriptReported {
                            workflow_id: workflow.id.clone(),
                            message: output
                                .response
                                .message
                                .clone()
                                .unwrap_or_else(|| "unspecified failure".to_string()),
                        },
                    );
                    if let Ok(degraded) = &outcome {
                        self.note_warnings(degraded);
                    }
                    outcome
                }
            }
        }
    }
}

/// Returns the cached discovery result while it is within
/// [`WORKFLOW_DISCOVERY_CACHE_TTL`], otherwise rescans both roots and
/// refreshes the cache.
fn cached_or_discover_workflows(
    app_handle: &tauri::AppHandle,
    cache: &Mutex<Option<WorkflowDiscoveryCache>>,
) -> WorkflowDiscoveryResult {
    if let Some(entry) = cache.lock().unwrap().as_ref()
        && entry.refreshed_at.elapsed() < WORKFLOW_DISCOVERY_CACHE_TTL
    {
        return entry.result.clone();
    }
    let result = discover_with_app_roots(app_handle);
    *cache.lock().unwrap() = Some(WorkflowDiscoveryCache {
        result: result.clone(),
        refreshed_at: Instant::now(),
    });
    result
}

/// The event name every typed workflow lifecycle event is emitted under.
pub const WORKFLOW_PROGRESS_EVENT: &str = "workflow-progress";

/// Builds the per-export workflow engine used by both GUI and headless
/// exports. Returns `Ok(None)` when no workflows are selected — the only
/// overhead for a no-workflow export is this empty-selection check, with no
/// discovery scan, interpreter probe, or subprocess. Selection errors are
/// reported before any image work starts. Emits a `Discovering` event before
/// the discovery scan so the UI can attribute that wait to workflow setup.
#[allow(clippy::too_many_arguments)]
pub fn prepare_export_workflow_engine(
    selected_ids: &[String],
    discovery_cache: &Mutex<Option<WorkflowDiscoveryCache>>,
    gate: &WorkflowConcurrencyGate,
    app_handle: &tauri::AppHandle,
    file_format: &str,
    jpeg_quality: u8,
    keep_metadata: bool,
    strip_gps: bool,
    total: u64,
) -> Result<Option<Arc<ExportWorkflowEngine>>, String> {
    if selected_ids.is_empty() {
        return Ok(None);
    }
    let run_id = generate_workflow_run_id();
    let event_app = app_handle.clone();
    let sink: WorkflowProgressSink = Arc::new(move |event: WorkflowProgressEvent| {
        let _ = event_app.emit(WORKFLOW_PROGRESS_EVENT, &event);
    });
    sink(WorkflowProgressEvent {
        run_id: run_id.clone(),
        phase: WorkflowProgressPhase::Discovering,
        workflow_id: None,
        source_path: None,
        index: 0,
        total,
        timeout_seconds: None,
        warning_count: 0,
    });
    let discovery = cached_or_discover_workflows(app_handle, discovery_cache);
    let plan = plan_export_workflows_with_run_id(run_id, selected_ids, &discovery)
        .map_err(|error| error.to_string())?;
    let Some(plan) = plan else {
        return Ok(None);
    };
    // Canonical roots for spawn-time script identity revalidation; roots
    // that cannot be canonicalized are dropped, and an empty list makes the
    // engine reject every script (fail closed).
    let (bundled_root, user_root) = workflow_discovery_roots(app_handle);
    let workflow_roots = [bundled_root, user_root]
        .into_iter()
        .flatten()
        .filter_map(|root| root.canonicalize().ok().map(|root| strip_verbatim(&root)))
        .collect::<Vec<_>>();
    let engine = ExportWorkflowEngine::new(
        plan,
        Arc::new(CommandWorkflowRunner),
        Arc::new(CommandRuntimeProber::default()),
        gate.clone(),
        WorkflowExportSettings {
            file_format: file_format.to_string(),
            jpeg_quality,
            keep_metadata,
            strip_gps,
        },
        std::env::temp_dir().join("rapidraw-workflows"),
        workflow_roots,
        total,
        Some(sink),
    );
    Ok(Some(Arc::new(engine)))
}

fn discover_with_app_roots(app_handle: &tauri::AppHandle) -> WorkflowDiscoveryResult {
    let (bundled_root, user_root) = workflow_discovery_roots(app_handle);
    let prober = CommandRuntimeProber::default();
    discover_workflows(&WorkflowDiscoveryOptions {
        bundled_root,
        user_root,
        platform: discovery_probe_platform(),
        prober: &prober,
    })
}

/// The bundled and user workflow roots shared by discovery, the GUI listing
/// commands, and the headless `--list-workflows` command. Roots are `None`
/// when the host cannot resolve them; missing directories surface as
/// diagnostics inside the discovery result instead.
pub fn workflow_discovery_roots(
    app_handle: &tauri::AppHandle,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let bundled_root = app_handle
        .path()
        .resolve("resources/workflows", tauri::path::BaseDirectory::Resource)
        .ok();
    let user_root = app_handle
        .path()
        .home_dir()
        .ok()
        .map(|home| home.join(".rapidraw").join("workflows"));
    (bundled_root, user_root)
}

fn listing_phase_label(phase: WorkflowPhase) -> &'static str {
    match phase {
        WorkflowPhase::PostImage => "postImage",
        WorkflowPhase::PostBatch => "postBatch",
    }
}

fn listing_language_label(language: WorkflowLanguage) -> &'static str {
    match language {
        WorkflowLanguage::Python => "python",
        WorkflowLanguage::JavaScript => "javascript",
    }
}

fn listing_source_label(source: WorkflowSource) -> &'static str {
    match source {
        WorkflowSource::Bundled => "bundled",
        WorkflowSource::User => "user",
    }
}

fn listing_runtime_label(workflow: &DiscoveredWorkflow) -> String {
    if workflow.runtime.available {
        match (&workflow.runtime.executable, &workflow.runtime.version) {
            (Some(executable), Some(version)) => format!("{executable} {version}"),
            (Some(executable), None) => executable.clone(),
            (None, _) => listing_language_label(workflow.language).to_string(),
        }
    } else {
        format!("{} unavailable", listing_language_label(workflow.language))
    }
}

fn pad(text: &str, width: usize) -> String {
    let mut padded = text.to_string();
    for _ in text.chars().count()..width {
        padded.push(' ');
    }
    padded
}

/// Formats the deterministic `--list-workflows` output: the scanned roots,
/// one row per workflow with id, phase, runtime availability, and source,
/// unselectable entries with their reasons, scan diagnostics, and a count.
/// Rows keep registry order (`order`, then id); column widths derive from the
/// content, so the same registry always produces the same text.
pub fn format_workflow_listing(
    bundled_root: Option<&Path>,
    user_root: Option<&Path>,
    result: &WorkflowDiscoveryResult,
) -> String {
    let bundled_root = bundled_root.map(strip_verbatim);
    let user_root = user_root.map(strip_verbatim);
    let mut lines: Vec<String> = Vec::new();
    match bundled_root {
        Some(root) => lines.push(format!("Bundled workflows directory: {}", root.display())),
        None => lines.push("Bundled workflows directory: (unresolvable)".to_string()),
    }
    match user_root {
        Some(root) => lines.push(format!("User workflows directory: {}", root.display())),
        None => lines.push("User workflows directory: (unresolvable)".to_string()),
    }
    lines.push(String::new());

    if result.workflows.is_empty() {
        lines.push("No workflows found.".to_string());
    } else {
        let id_width = result
            .workflows
            .iter()
            .map(|workflow| workflow.id.chars().count())
            .chain(std::iter::once(2))
            .max()
            .unwrap_or(2);
        let runtime_width = result
            .workflows
            .iter()
            .map(|workflow| listing_runtime_label(workflow).chars().count())
            .chain(std::iter::once(7))
            .max()
            .unwrap_or(7);
        lines.push(format!(
            "{} {} {} {}",
            pad("ID", id_width),
            pad("PHASE", 9),
            pad("RUNTIME", runtime_width),
            "SOURCE"
        ));
        for workflow in &result.workflows {
            lines.push(format!(
                "{} {} {} {}",
                pad(&workflow.id, id_width),
                pad(listing_phase_label(workflow.phase), 9),
                pad(&listing_runtime_label(workflow), runtime_width),
                listing_source_label(workflow.source)
            ));
        }

        let unselectable: Vec<&DiscoveredWorkflow> = result
            .workflows
            .iter()
            .filter(|workflow| !workflow.selectable)
            .collect();
        if !unselectable.is_empty() {
            lines.push(String::new());
            lines.push("Unselectable (invalid metadata; fix the sidecar to use them):".to_string());
            for workflow in unselectable {
                let reasons: Vec<&str> = workflow
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect();
                lines.push(format!(
                    "- {}: {}",
                    workflow.id,
                    if reasons.is_empty() {
                        "invalid metadata".to_string()
                    } else {
                        reasons.join("; ")
                    }
                ));
            }
        }
        lines.push(String::new());
        lines.push(format!("{} workflow(s) listed.", result.workflows.len()));
    }

    if !result.diagnostics.is_empty() {
        lines.push(String::new());
        lines.push("Diagnostics:".to_string());
        for diagnostic in &result.diagnostics {
            lines.push(format!("- {}: {}", diagnostic.code, diagnostic.message));
        }
    }

    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// One fresh, uncached discovery scan formatted for the headless
/// `--list-workflows` command. Blocking by design: interpreter probes run
/// inline with the documented per-candidate timeout.
pub fn headless_workflow_listing(app_handle: &tauri::AppHandle) -> String {
    let (bundled_root, user_root) = workflow_discovery_roots(app_handle);
    let prober = CommandRuntimeProber::default();
    let result = discover_workflows(&WorkflowDiscoveryOptions {
        bundled_root: bundled_root.clone(),
        user_root: user_root.clone(),
        platform: discovery_probe_platform(),
        prober: &prober,
    });
    format_workflow_listing(bundled_root.as_deref(), user_root.as_deref(), &result)
}

fn headless_summary_status_label(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Succeeded => "succeeded",
        WorkflowRunStatus::Warned => "warned",
        WorkflowRunStatus::Failed | WorkflowRunStatus::TimedOut => "degraded",
        WorkflowRunStatus::Cancelled => "cancelled",
    }
}

/// Formats the per-run workflow outcome lines and closing counters printed
/// by headless exports when the terminal `export-result` detail arrives.
/// Returns an empty string when the run selected no workflows, so plain
/// headless exports keep their existing output.
pub fn format_headless_workflow_summary(detail: &ExportResultDetail) -> String {
    if detail.workflow_runs.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for run in &detail.workflow_runs {
        let scope = run
            .source_path
            .as_deref()
            .map_or_else(String::new, |source| {
                let name = Path::new(source).file_name().map_or_else(
                    || source.to_string(),
                    |name| name.to_string_lossy().to_string(),
                );
                format!(" for {name}")
            });
        let mut line = format!(
            "Workflow '{}' {}{}",
            run.workflow_id,
            headless_summary_status_label(run.status),
            scope
        );
        if let Some(message) = run.message.as_deref()
            && !message.is_empty()
        {
            line.push_str(": ");
            line.push_str(message);
        }
        lines.push(line);
        for warning in &run.warnings {
            lines.push(format!("  warning: {warning}"));
        }
    }
    let mut succeeded = 0u64;
    let mut warned = 0u64;
    let mut degraded = 0u64;
    let mut cancelled = 0u64;
    for run in &detail.workflow_runs {
        match run.status {
            WorkflowRunStatus::Succeeded => succeeded += 1,
            WorkflowRunStatus::Warned => warned += 1,
            WorkflowRunStatus::Failed | WorkflowRunStatus::TimedOut => degraded += 1,
            WorkflowRunStatus::Cancelled => cancelled += 1,
        }
    }
    lines.push(format!(
        "Workflow runs: {} succeeded, {} warned, {} degraded, {} cancelled.",
        succeeded, warned, degraded, cancelled
    ));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Lists workflows, reusing a cached scan that is at most
/// [`WORKFLOW_DISCOVERY_CACHE_TTL`] old.
#[tauri::command]
pub fn discover_export_workflows(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> WorkflowDiscoveryResult {
    cached_or_discover_workflows(&app_handle, &state.workflow_discovery_cache)
}

/// Rescans both roots immediately and reports what changed since the cached run.
#[tauri::command]
pub fn refresh_export_workflows(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> WorkflowRefreshReport {
    let previous = state
        .workflow_discovery_cache
        .lock()
        .unwrap()
        .as_ref()
        .map(|cache| cache.result.clone());
    let result = discover_with_app_roots(&app_handle);
    let delta = match previous {
        Some(previous) => diff_workflow_discovery(&previous, &result),
        None => {
            let mut added_workflow_ids: Vec<String> = result
                .workflows
                .iter()
                .map(|workflow| workflow.id.clone())
                .collect();
            added_workflow_ids.sort();
            WorkflowRefreshDelta {
                added_workflow_ids,
                removed_workflow_ids: Vec::new(),
                availability_changes: Vec::new(),
            }
        }
    };
    *state.workflow_discovery_cache.lock().unwrap() = Some(WorkflowDiscoveryCache {
        result: result.clone(),
        refreshed_at: Instant::now(),
    });
    WorkflowRefreshReport {
        result,
        added_workflow_ids: delta.added_workflow_ids,
        removed_workflow_ids: delta.removed_workflow_ids,
        availability_changes: delta.availability_changes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> WorkflowRequest {
        WorkflowRequest {
            protocol_version: WORKFLOW_PROTOCOL_VERSION,
            run_id: "run-1".to_string(),
            workflow_id: "receipt".to_string(),
            phase: WorkflowPhase::PostBatch,
            source_path: None,
            exported_path: None,
            artifacts: Vec::new(),
            selected_items: vec![WorkflowItem {
                source_path: "source.nef".to_string(),
                exported_path: None,
                artifacts: Vec::new(),
                error: None,
            }],
            exported_items: vec![WorkflowItem {
                source_path: "source.nef".to_string(),
                exported_path: Some("output.jpg".to_string()),
                artifacts: Vec::new(),
                error: None,
            }],
            export_settings: WorkflowExportSettings {
                file_format: "jpeg".to_string(),
                jpeg_quality: 90,
                keep_metadata: true,
                strip_gps: false,
            },
            index: None,
            total: 1,
            workspace_temp_directory: "temp".to_string(),
        }
    }

    #[test]
    fn typed_contracts_workflow_request_round_trips_as_camel_case() {
        let value = serde_json::to_value(request()).expect("serialize");
        assert_eq!(value["protocolVersion"], WORKFLOW_PROTOCOL_VERSION);
        assert_eq!(value["workflowId"], "receipt");
        assert_eq!(value["phase"], "postBatch");

        let decoded: WorkflowRequest = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, request());
    }

    #[test]
    fn typed_contracts_unknown_workflow_protocol_is_actionable() {
        let mut value = serde_json::to_value(request()).expect("serialize");
        value["protocolVersion"] = serde_json::json!(2);

        let error = serde_json::from_value::<WorkflowRequest>(value)
            .expect_err("unsupported protocol must fail")
            .to_string();

        assert!(error.contains("unsupported workflow protocol version 2"));
        assert!(error.contains("expected 1"));
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("workflows")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()))
    }

    #[test]
    fn workflow_metadata_sidecar_is_optional_field_by_field() {
        let empty: WorkflowMetadata = serde_json::from_str("{}").expect("empty sidecar is valid");
        assert_eq!(empty, WorkflowMetadata::default());

        let full: WorkflowMetadata = serde_json::from_str(
            r#"{
                "id": "example-post-batch-receipt",
                "displayName": "Example post-batch receipt",
                "description": "Writes a receipt.",
                "phase": "postBatch",
                "order": 100,
                "timeoutSeconds": 30,
                "onError": "warn"
            }"#,
        )
        .expect("full sidecar parses");
        assert_eq!(full.id.as_deref(), Some("example-post-batch-receipt"));
        assert_eq!(
            full.display_name.as_deref(),
            Some("Example post-batch receipt")
        );
        assert_eq!(full.phase, Some(WorkflowPhase::PostBatch));
        assert_eq!(full.order, Some(100));
        assert_eq!(full.timeout_seconds, Some(30));
        assert_eq!(full.on_error, Some(WorkflowErrorPolicy::Warn));
        assert!(full.validate().is_empty());

        let round_trip: WorkflowMetadata =
            serde_json::from_value(serde_json::to_value(&full).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round_trip, full);
    }

    #[test]
    fn workflow_metadata_validation_reports_typed_diagnostics() {
        let invalid: WorkflowMetadata = serde_json::from_str(
            r#"{
                "id": "Bad Id!",
                "displayName": "x",
                "order": -1,
                "timeoutSeconds": 601
            }"#,
        )
        .expect("values parse as JSON");
        let diagnostics = invalid.validate();
        let codes: Vec<&str> = diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(codes.contains(&"workflow.metadata.id.invalid"));
        assert!(codes.contains(&"workflow.metadata.order.outOfRange"));
        assert!(codes.contains(&"workflow.metadata.timeoutSeconds.outOfRange"));

        let boundary: WorkflowMetadata =
            serde_json::from_str(r#"{ "id": "ok-id", "order": 0, "timeoutSeconds": 600 }"#)
                .expect("boundary values parse");
        assert!(boundary.validate().is_empty());
    }

    #[test]
    fn workflow_direct_file_defaults_require_no_manifest() {
        let python =
            derive_workflow_defaults("Sharpen_with_AI.py").expect("direct .py file is valid");
        assert_eq!(python.id, "sharpen-with-ai");
        assert_eq!(python.display_name, "Sharpen with AI");
        assert_eq!(python.language, WorkflowLanguage::Python);
        assert_eq!(python.phase, WorkflowPhase::PostBatch);
        assert_eq!(python.order, WORKFLOW_DEFAULT_ORDER);
        assert_eq!(python.timeout_seconds, WORKFLOW_DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(python.on_error, WorkflowErrorPolicy::Warn);

        let node = derive_workflow_defaults("backup receipts.JS")
            .expect("direct .js file with any extension case is valid");
        assert_eq!(node.language, WorkflowLanguage::JavaScript);
        assert_eq!(node.id, "backup-receipts");
        assert_eq!(node.display_name, "backup receipts");

        let long_stem = format!("{}.py", "x".repeat(100));
        let truncated =
            derive_workflow_defaults(&long_stem).expect("long direct file is still valid");
        assert!(truncated.id.len() <= 64);

        assert!(derive_workflow_defaults("readme.txt").is_none());
        assert!(derive_workflow_defaults("example_post_batch.js.rapidraw.json").is_none());
        assert!(derive_workflow_defaults("noext").is_none());
    }

    #[test]
    fn workflow_example_documents_validate_against_serde_models() {
        let post_image: WorkflowRequest = serde_json::from_str(&fixture("post-image-request.json"))
            .expect("postImage example validates");
        assert_eq!(post_image.phase, WorkflowPhase::PostImage);
        assert_eq!(
            post_image.source_path.as_deref(),
            Some("/home/photographer/Pictures/raw/DSC_0001.NEF")
        );
        assert!(post_image.exported_path.is_some());
        assert_eq!(post_image.artifacts.len(), 1);
        assert_eq!(post_image.index, Some(0));
        assert_eq!(post_image.total, 12);
        assert!(post_image.selected_items.is_empty());
        assert!(post_image.exported_items.is_empty());

        let post_batch: WorkflowRequest = serde_json::from_str(&fixture("post-batch-request.json"))
            .expect("postBatch example validates");
        assert_eq!(post_batch.phase, WorkflowPhase::PostBatch);
        assert!(post_batch.source_path.is_none());
        assert!(post_batch.exported_path.is_none());
        assert!(post_batch.artifacts.is_empty());
        assert_eq!(post_batch.index, None);
        assert_eq!(post_batch.selected_items.len(), 2);
        assert_eq!(post_batch.exported_items.len(), 1);
        assert!(post_batch.exported_items[0].error.is_none());

        let response: WorkflowResponse =
            serde_json::from_str(&fixture("response.json")).expect("response example validates");
        assert!(response.ok);
        assert_eq!(
            response.produced_artifact_paths,
            vec!["receipt.json".to_string()]
        );
    }

    #[test]
    fn workflow_fixture_sidecar_metadata_parses_and_validates() {
        let metadata: WorkflowMetadata =
            serde_json::from_str(&fixture("example_post_batch.js.rapidraw.json"))
                .expect("sidecar example parses");
        let diagnostics = metadata.validate();
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        assert_eq!(metadata.id.as_deref(), Some("example-post-batch-receipt"));
    }

    #[test]
    fn workflow_malformed_output_and_nonzero_exit_are_distinct_errors() {
        let malformed = WorkflowExecutionError::MalformedOutput {
            reason: "stdout was not valid JSON".to_string(),
        };
        let nonzero = WorkflowExecutionError::NonZeroExit { code: 2 };

        assert_ne!(malformed, nonzero);
        assert_eq!(malformed.status(), WorkflowRunStatus::Failed);
        assert_eq!(nonzero.status(), WorkflowRunStatus::Failed);
        assert_eq!(
            WorkflowExecutionError::TimedOut.status(),
            WorkflowRunStatus::TimedOut
        );
        assert_eq!(
            WorkflowExecutionError::Cancelled.status(),
            WorkflowRunStatus::Cancelled
        );
        assert!(malformed.to_string().contains("malformed workflow output"));
        assert!(nonzero.to_string().contains("exit code 2"));
    }

    #[test]
    fn workflow_response_tolerates_missing_optional_collections() {
        let minimal: WorkflowResponse =
            serde_json::from_str(r#"{"ok": true}"#).expect("minimal response is script-friendly");
        assert!(minimal.ok);
        assert_eq!(minimal.message, None);
        assert!(minimal.warnings.is_empty());
        assert!(minimal.produced_artifact_paths.is_empty());
    }

    #[test]
    fn workflow_progress_events_serialize_image_and_workflow_phases_as_camel_case() {
        let rendering = WorkflowProgressEvent {
            run_id: "run-1".to_string(),
            phase: WorkflowProgressPhase::Rendering,
            workflow_id: None,
            source_path: Some("raw/DSC_0001.NEF".to_string()),
            index: 0,
            total: 3,
            timeout_seconds: None,
            warning_count: 0,
        };
        let value = serde_json::to_value(&rendering).expect("serialize rendering");
        assert_eq!(value["phase"], "rendering");
        assert_eq!(value["runId"], "run-1");
        assert_eq!(value["sourcePath"], "raw/DSC_0001.NEF");
        assert_eq!(value["timeoutSeconds"], serde_json::Value::Null);

        let mut writing = rendering.clone();
        writing.phase = WorkflowProgressPhase::Writing;
        writing.source_path = Some("raw/DSC_0002.NEF".to_string());
        writing.index = 1;
        assert_eq!(
            serde_json::to_value(&writing).expect("serialize writing")["phase"],
            "writing"
        );

        let mut running = rendering;
        running.phase = WorkflowProgressPhase::RunningPostImage;
        running.workflow_id = Some("receipt".to_string());
        running.timeout_seconds = Some(30);
        running.warning_count = 2;
        let value = serde_json::to_value(&running).expect("serialize running");
        assert_eq!(value["phase"], "runningPostImage");
        assert_eq!(value["workflowId"], "receipt");
        assert_eq!(value["timeoutSeconds"], 30);
        assert_eq!(value["warningCount"], 2);

        for (phase, label) in [
            (WorkflowProgressPhase::Discovering, "discovering"),
            (WorkflowProgressPhase::Waiting, "waiting"),
            (WorkflowProgressPhase::RunningPostBatch, "runningPostBatch"),
            (WorkflowProgressPhase::Cancelling, "cancelling"),
            (WorkflowProgressPhase::Complete, "complete"),
            (WorkflowProgressPhase::Cancelled, "cancelled"),
        ] {
            let mut event = running.clone();
            event.phase = phase;
            assert_eq!(
                serde_json::to_value(&event).expect("serialize")["phase"],
                label,
                "{phase:?} must keep its wire label"
            );
        }
    }

    #[test]
    fn workflow_stderr_excerpt_redaction_bounds_text_and_strips_home_paths() {
        let home = if cfg!(windows) {
            r"C:\Users\photographer"
        } else {
            "/home/photographer"
        };
        let text = format!("reading {home}/library/raw/DSC_0001.NEF\nsecond line");
        let redacted = redact_workflow_text(&text, Some(home));
        assert!(redacted.contains("~/library/raw"), "{redacted}");
        assert!(!redacted.contains(home), "{redacted}");
        assert!(redacted.contains("second line"), "{redacted}");

        // Without a home the text passes through untouched.
        let plain = redact_workflow_text("no paths here", None);
        assert_eq!(plain, "no paths here");

        // Excerpts are bounded with an explicit truncation marker.
        let flood = "x".repeat(5_000);
        let bounded = redact_workflow_text(&flood, Some(home));
        assert!(
            bounded.chars().count() <= WORKFLOW_STDERR_EXCERPT_CHARS + 20,
            "bounded excerpt: {} chars",
            bounded.chars().count()
        );
        assert!(
            bounded.ends_with(WORKFLOW_STDERR_EXCERPT_TRUNCATION_MARKER),
            "{bounded}"
        );

        // Both slash variants of the home path are redacted.
        let windows_style = redact_workflow_text(
            &format!("{}/mixed/style.nef", home.replace('\\', "/")),
            Some(home),
        );
        assert!(!windows_style.contains(home), "{windows_style}");
    }

    fn listing_workflow(
        id: &str,
        phase: WorkflowPhase,
        language: WorkflowLanguage,
        source: WorkflowSource,
        selectable: bool,
        available: bool,
    ) -> DiscoveredWorkflow {
        DiscoveredWorkflow {
            id: id.to_string(),
            display_name: id.to_string(),
            description: None,
            language,
            phase,
            order: 100,
            timeout_seconds: 60,
            on_error: WorkflowErrorPolicy::Warn,
            source,
            script_path: format!("/workflows/{id}.py"),
            selectable,
            runtime: WorkflowRuntime {
                available,
                executable: available.then(|| "python".to_string()),
                version: available.then(|| "Python 3.13.0".to_string()),
                unavailable_reason: (!available).then(|| "no python runtime".to_string()),
            },
            diagnostics: if selectable {
                Vec::new()
            } else {
                vec![WorkflowDiagnostic {
                    code: "workflow.metadata.order.outOfRange".to_string(),
                    message: "order must be 0..=1000".to_string(),
                }]
            },
        }
    }

    #[test]
    fn workflow_listing_shows_id_phase_runtime_and_source() {
        let result = WorkflowDiscoveryResult {
            workflows: vec![
                listing_workflow(
                    "example-post-image",
                    WorkflowPhase::PostImage,
                    WorkflowLanguage::Python,
                    WorkflowSource::Bundled,
                    true,
                    true,
                ),
                listing_workflow(
                    "example-post-batch-receipt",
                    WorkflowPhase::PostBatch,
                    WorkflowLanguage::JavaScript,
                    WorkflowSource::Bundled,
                    true,
                    true,
                ),
                listing_workflow(
                    "my-backup",
                    WorkflowPhase::PostBatch,
                    WorkflowLanguage::Python,
                    WorkflowSource::User,
                    true,
                    false,
                ),
            ],
            diagnostics: Vec::new(),
        };

        let listing = format_workflow_listing(
            Some(Path::new("/app/resources/workflows")),
            Some(Path::new("/home/user/.rapidraw/workflows")),
            &result,
        );

        assert!(listing.contains("Bundled workflows directory: /app/resources/workflows"));
        assert!(listing.contains("User workflows directory: /home/user/.rapidraw/workflows"));
        assert!(listing.contains("ID"));
        assert!(listing.contains("PHASE"));
        assert!(listing.contains("RUNTIME"));
        assert!(listing.contains("SOURCE"));

        let row = |id: &str| {
            listing
                .lines()
                .find(|line| line.starts_with(id))
                .unwrap_or_else(|| panic!("listing has no row for {id}:\n{listing}"))
                .to_string()
        };
        let post_image = row("example-post-image");
        assert!(post_image.contains("postImage"));
        assert!(post_image.contains("python Python 3.13.0"));
        assert!(post_image.contains("bundled"));
        let post_batch = row("example-post-batch-receipt");
        assert!(post_batch.contains("postBatch"));
        assert!(post_batch.contains("bundled"));
        let unavailable = row("my-backup");
        assert!(unavailable.contains("postBatch"));
        assert!(unavailable.contains("python unavailable"));
        assert!(unavailable.contains("user"));
        // The listing keeps registry order as given (discovery sorts by
        // `order`, then id, before the formatter ever sees it).
        let ordered_ids: Vec<&str> = listing
            .lines()
            .filter(|line| line.starts_with("example-") || line.starts_with("my-"))
            .map(|line| line.split_whitespace().next().unwrap_or(""))
            .collect();
        assert_eq!(
            ordered_ids,
            vec![
                "example-post-image",
                "example-post-batch-receipt",
                "my-backup"
            ]
        );
        assert!(listing.contains("3 workflow(s) listed."));
        // Same registry, same text: the listing is deterministic.
        assert_eq!(
            listing,
            format_workflow_listing(
                Some(Path::new("/app/resources/workflows")),
                Some(Path::new("/home/user/.rapidraw/workflows")),
                &result
            )
        );
    }

    #[test]
    fn workflow_listing_marks_unselectable_entries_and_prints_diagnostics() {
        let result = WorkflowDiscoveryResult {
            workflows: vec![listing_workflow(
                "broken",
                WorkflowPhase::PostBatch,
                WorkflowLanguage::Python,
                WorkflowSource::User,
                false,
                true,
            )],
            diagnostics: vec![WorkflowDiagnostic {
                code: "workflow.discovery.root.missing".to_string(),
                message: "the bundled workflows root does not exist".to_string(),
            }],
        };

        let listing = format_workflow_listing(
            None,
            Some(Path::new("/home/u/.rapidraw/workflows")),
            &result,
        );

        assert!(listing.contains("Bundled workflows directory: (unresolvable)"));
        assert!(listing.contains("broken"));
        assert!(listing.contains("Unselectable"));
        assert!(listing.contains("- broken: order must be 0..=1000"));
        assert!(listing.contains("Diagnostics:"));
        assert!(listing.contains(
            "- workflow.discovery.root.missing: the bundled workflows root does not exist"
        ));
    }

    #[test]
    fn workflow_listing_reports_an_empty_registry() {
        let empty = WorkflowDiscoveryResult {
            workflows: Vec::new(),
            diagnostics: Vec::new(),
        };
        let listing = format_workflow_listing(None, None, &empty);
        assert!(listing.contains("No workflows found."));
        assert!(listing.contains("Bundled workflows directory: (unresolvable)"));
        assert!(listing.contains("User workflows directory: (unresolvable)"));
    }

    #[test]
    fn headless_summary_formats_run_outcomes_and_counts() {
        let detail = summarize_export_results(
            "run-1",
            false,
            vec![ExportItemOutcome {
                source_path: "raw/DSC_0001.NEF".to_string(),
                exported_path: Some("out/DSC_0001.jpg".to_string()),
                error: None,
            }],
            vec![
                WorkflowRunResult {
                    workflow_id: "receipt".to_string(),
                    source_path: Some("raw/DSC_0001.NEF".to_string()),
                    status: WorkflowRunStatus::Succeeded,
                    message: Some("wrote receipt".to_string()),
                    warnings: Vec::new(),
                    produced_artifacts: Vec::new(),
                    stderr_excerpt: None,
                },
                WorkflowRunResult {
                    workflow_id: "uploader".to_string(),
                    source_path: None,
                    status: WorkflowRunStatus::Failed,
                    message: Some("archive offline".to_string()),
                    warnings: vec!["retry scheduled".to_string()],
                    produced_artifacts: Vec::new(),
                    stderr_excerpt: None,
                },
                WorkflowRunResult {
                    workflow_id: "chatty".to_string(),
                    source_path: None,
                    status: WorkflowRunStatus::Warned,
                    message: None,
                    warnings: vec!["stale sidecar".to_string()],
                    produced_artifacts: Vec::new(),
                    stderr_excerpt: None,
                },
            ],
        );

        let summary = format_headless_workflow_summary(&detail);
        assert!(summary.contains("Workflow 'receipt' succeeded for DSC_0001.NEF: wrote receipt"));
        assert!(summary.contains("Workflow 'uploader' degraded: archive offline"));
        assert!(summary.contains("  warning: retry scheduled"));
        assert!(summary.contains("Workflow 'chatty' warned"));
        assert!(summary.contains("  warning: stale sidecar"));
        assert!(summary.contains("Workflow runs: 1 succeeded, 1 warned, 1 degraded, 0 cancelled."));
    }

    #[test]
    fn headless_summary_is_empty_without_workflow_runs() {
        let detail = summarize_export_results(
            "run-1",
            false,
            vec![ExportItemOutcome {
                source_path: "raw/DSC_0001.NEF".to_string(),
                exported_path: Some("out/DSC_0001.jpg".to_string()),
                error: None,
            }],
            Vec::new(),
        );
        assert_eq!(format_headless_workflow_summary(&detail), "");
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeProber {
        attempts: Mutex<Vec<String>>,
        available: Mutex<HashMap<String, String>>,
    }

    impl FakeProber {
        fn with(program: &str, version: &str) -> Self {
            let prober = Self::default();
            prober
                .available
                .lock()
                .unwrap()
                .insert(program.to_string(), version.to_string());
            prober
        }

        fn attempts(&self) -> Vec<String> {
            self.attempts.lock().unwrap().clone()
        }
    }

    impl RuntimeProber for FakeProber {
        fn probe(&self, candidate: &RuntimeCandidate) -> Result<String, String> {
            let invocation = format!("{} {}", candidate.program, candidate.args.join(" "));
            self.attempts.lock().unwrap().push(invocation);
            self.available
                .lock()
                .unwrap()
                .get(&candidate.program)
                .cloned()
                .ok_or_else(|| format!("{} is not available", candidate.program))
        }
    }

    fn write_file(root: &Path, name: &str, contents: &str) -> PathBuf {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        path
    }

    fn options<'a>(
        bundled_root: Option<&Path>,
        user_root: Option<&Path>,
        platform: ProbePlatform,
        prober: &'a FakeProber,
    ) -> WorkflowDiscoveryOptions<'a> {
        WorkflowDiscoveryOptions {
            bundled_root: bundled_root.map(Path::to_path_buf),
            user_root: user_root.map(Path::to_path_buf),
            platform,
            prober,
        }
    }

    fn diagnostic_codes(result: &WorkflowDiscoveryResult) -> Vec<&str> {
        result.diagnostics.iter().map(|d| d.code.as_str()).collect()
    }

    #[test]
    fn workflow_discovery_lists_direct_files_with_derived_defaults() {
        let bundled = tempfile::tempdir().expect("bundled tempdir");
        write_file(bundled.path(), "Sharpen_with_AI.py", "print('hi')");
        write_file(bundled.path(), "backup receipts.JS", "console.log('hi')");
        let prober = FakeProber::default();
        {
            let mut available = prober.available.lock().unwrap();
            available.insert("python3".to_string(), "Python 3.12.0".to_string());
            available.insert("node".to_string(), "v22.1.0".to_string());
        }

        let result = discover_workflows(&options(
            Some(bundled.path()),
            None,
            ProbePlatform::Unix,
            &prober,
        ));

        // the unresolved user root is reported but does not fail discovery
        assert_eq!(
            diagnostic_codes(&result),
            vec!["workflow.discovery.root.missing"]
        );
        assert!(
            result.diagnostics[0].message.contains("user"),
            "{:?}",
            result.diagnostics
        );
        let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["backup-receipts", "sharpen-with-ai"]);

        let python = &result.workflows[1];
        assert_eq!(python.display_name, "Sharpen with AI");
        assert_eq!(python.language, WorkflowLanguage::Python);
        assert_eq!(python.phase, WorkflowPhase::PostBatch);
        assert_eq!(python.order, WORKFLOW_DEFAULT_ORDER);
        assert_eq!(python.timeout_seconds, WORKFLOW_DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(python.on_error, WorkflowErrorPolicy::Warn);
        assert_eq!(python.source, WorkflowSource::Bundled);
        assert!(python.selectable);
        assert!(python.diagnostics.is_empty());
        assert!(python.script_path.ends_with("Sharpen_with_AI.py"));

        let node = &result.workflows[0];
        assert_eq!(node.language, WorkflowLanguage::JavaScript);
        assert_eq!(
            node.runtime,
            WorkflowRuntime {
                available: true,
                executable: Some("node".to_string()),
                version: Some("v22.1.0".to_string()),
                unavailable_reason: None,
            }
        );

        let serialized = serde_json::to_value(&result).expect("serialize discovery");
        assert_eq!(serialized["workflows"][1]["scriptPath"], python.script_path);
        assert_eq!(serialized["workflows"][1]["selectable"], true);
    }

    #[test]
    fn workflow_discovery_applies_sidecar_overrides_and_reports_invalid_metadata() {
        let user = tempfile::tempdir().expect("user tempdir");
        write_file(user.path(), "receipt.js", "console.log('hi')");
        write_file(
            user.path(),
            "receipt.js.rapidraw.json",
            r#"{
                "id": "example-post-batch-receipt",
                "displayName": "Example receipt",
                "description": "Writes a receipt.",
                "phase": "postImage",
                "order": 5,
                "timeoutSeconds": 30,
                "onError": "fail"
            }"#,
        );
        write_file(user.path(), "broken.py", "print('hi')");
        write_file(
            user.path(),
            "broken.py.rapidraw.json",
            r#"{ "id": "Bad Id!", "order": -1 }"#,
        );
        write_file(user.path(), "worse.js", "console.log('hi')");
        write_file(user.path(), "worse.js.rapidraw.json", "{ not json");
        let prober = FakeProber::with("python3", "Python 3.12.0");

        let result = discover_workflows(&options(
            None,
            Some(user.path()),
            ProbePlatform::Unix,
            &prober,
        ));

        let receipt = result
            .workflows
            .iter()
            .find(|w| w.id == "example-post-batch-receipt")
            .expect("sidecar id overrides derived id");
        assert_eq!(receipt.display_name, "Example receipt");
        assert_eq!(receipt.phase, WorkflowPhase::PostImage);
        assert_eq!(receipt.order, 5);
        assert_eq!(receipt.timeout_seconds, 30);
        assert_eq!(receipt.on_error, WorkflowErrorPolicy::Fail);
        assert_eq!(receipt.source, WorkflowSource::User);
        assert!(receipt.selectable);

        let broken = result
            .workflows
            .iter()
            .find(|w| w.id == "broken")
            .expect("invalid sidecar entry is still listed");
        assert!(
            !broken.selectable,
            "invalid metadata must not be selectable"
        );
        let codes: Vec<&str> = broken.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert!(codes.contains(&"workflow.metadata.id.invalid"), "{codes:?}");
        assert!(
            codes.contains(&"workflow.metadata.order.outOfRange"),
            "{codes:?}"
        );

        let worse = result
            .workflows
            .iter()
            .find(|w| w.id == "worse")
            .expect("malformed sidecar entry is still listed");
        assert!(!worse.selectable);
        assert!(
            diagnostic_codes(&result).contains(&"workflow.discovery.metadata.invalid"),
            "{:?}",
            result.diagnostics
        );
    }

    #[test]
    fn workflow_discovery_reports_missing_and_unreadable_roots() {
        let bundled = tempfile::tempdir().expect("bundled tempdir");
        write_file(bundled.path(), "only.py", "print('hi')");
        // A regular file where a directory is expected makes read_dir fail.
        let user_root_file = tempfile::NamedTempFile::new().expect("named temp file");
        let user_root = user_root_file.path().to_path_buf();
        let prober = FakeProber::with("python3", "Python 3.12.0");

        let result = discover_workflows(&options(
            Some(bundled.path()),
            Some(&user_root),
            ProbePlatform::Unix,
            &prober,
        ));

        // bundled workflows still discovered; unusable user root is only a diagnostic
        let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["only"]);
        assert!(
            diagnostic_codes(&result).contains(&"workflow.discovery.root.unreadable"),
            "{:?}",
            result.diagnostics
        );

        drop(user_root_file);
        let neither = discover_workflows(&options(None, None, ProbePlatform::Unix, &prober));
        assert!(neither.workflows.is_empty());
        let codes = diagnostic_codes(&neither);
        let missing = codes
            .iter()
            .filter(|c| **c == "workflow.discovery.root.missing")
            .count();
        assert_eq!(missing, 2, "both roots reported missing: {codes:?}");
        assert!(
            neither
                .diagnostics
                .iter()
                .any(|d| d.message.contains("bundled")),
            "{:?}",
            neither.diagnostics
        );
        assert!(
            neither
                .diagnostics
                .iter()
                .any(|d| d.message.contains("user")),
            "{:?}",
            neither.diagnostics
        );
    }

    #[test]
    fn workflow_discovery_rejects_symlink_entries_without_executing_them() {
        let user = tempfile::tempdir().expect("user tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        write_file(outside.path(), "evil.py", "print('pwned')");
        let link_path = user.path().join("linked.py");
        #[cfg(unix)]
        let link = std::os::unix::fs::symlink(outside.path().join("evil.py"), &link_path);
        #[cfg(windows)]
        let link = std::os::windows::fs::symlink_file(outside.path().join("evil.py"), &link_path);
        if let Err(error) = link {
            // Symlink creation requires privileges on some Windows setups; the
            // rejection path is still exercised wherever creation succeeds.
            eprintln!("skipping: symlink creation unavailable: {error}");
            return;
        }
        let prober = FakeProber::with("python3", "Python 3.12.0");

        let result = discover_workflows(&options(
            None,
            Some(user.path()),
            ProbePlatform::Unix,
            &prober,
        ));

        assert!(result.workflows.is_empty(), "{:?}", result.workflows);
        assert!(
            diagnostic_codes(&result).contains(&"workflow.discovery.symlink.rejected"),
            "{:?}",
            result.diagnostics
        );
        assert!(prober.attempts().is_empty());
    }

    #[test]
    fn workflow_discovery_duplicate_ids_and_user_override() {
        let bundled = tempfile::tempdir().expect("bundled tempdir");
        let user = tempfile::tempdir().expect("user tempdir");
        write_file(bundled.path(), "Sharpen with AI.py", "print('a')");
        write_file(bundled.path(), "sharpen_with_ai.py", "print('b')");
        write_file(user.path(), "sharpen_with_ai.py", "print('c')");
        let prober = FakeProber::with("python3", "Python 3.12.0");

        let result = discover_workflows(&options(
            Some(bundled.path()),
            Some(user.path()),
            ProbePlatform::Unix,
            &prober,
        ));

        let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["sharpen-with-ai"]);
        let winner = &result.workflows[0];
        assert_eq!(winner.source, WorkflowSource::User);
        assert!(Path::new(&winner.script_path).starts_with(user.path()));
        let codes = diagnostic_codes(&result);
        assert!(
            codes.contains(&"workflow.discovery.duplicate.id"),
            "{codes:?}"
        );
        assert!(
            codes.contains(&"workflow.discovery.override.user"),
            "{codes:?}"
        );
        let duplicate = result
            .diagnostics
            .iter()
            .find(|d| d.code == "workflow.discovery.duplicate.id")
            .expect("duplicate diagnostic");
        assert!(
            duplicate.message.contains("sharpen_with_ai.py"),
            "shadowed path reported: {}",
            duplicate.message
        );
    }

    #[test]
    fn workflow_discovery_reports_unsupported_extensions_and_orphan_sidecars() {
        let user = tempfile::tempdir().expect("user tempdir");
        write_file(user.path(), "readme.txt", "notes");
        write_file(
            user.path(),
            "orphaned.js.rapidraw.json",
            r#"{ "id": "orphan" }"#,
        );
        write_file(user.path(), ".hidden.py", "print('secret')");
        write_file(&user.path().join("nested"), "nested.py", "print('nested')");
        let prober = FakeProber::default();

        let result = discover_workflows(&options(
            None,
            Some(user.path()),
            ProbePlatform::Unix,
            &prober,
        ));

        assert!(result.workflows.is_empty(), "{:?}", result.workflows);
        let codes = diagnostic_codes(&result);
        assert!(
            codes.contains(&"workflow.discovery.extension.unsupported"),
            "{codes:?}"
        );
        assert!(
            codes.contains(&"workflow.discovery.sidecar.orphaned"),
            "{codes:?}"
        );
        let relevant: Vec<&str> = codes
            .iter()
            .copied()
            .filter(|code| *code != "workflow.discovery.root.missing")
            .collect();
        assert_eq!(
            relevant.len(),
            2,
            "dotfiles and directories are skipped silently: {codes:?}"
        );
        // No workflows means no interpreter probing at all.
        assert!(prober.attempts().is_empty());
    }

    #[test]
    fn workflow_discovery_probes_candidates_in_platform_order_with_argument_arrays() {
        let unix_root = tempfile::tempdir().expect("unix tempdir");
        write_file(unix_root.path(), "a.py", "print('a')");
        write_file(unix_root.path(), "b.js", "console.log('b')");

        let prober = FakeProber::default();
        {
            let mut available = prober.available.lock().unwrap();
            available.insert("python3".to_string(), "Python 3.12.0".to_string());
            available.insert("node".to_string(), "v22.1.0".to_string());
        }
        let result = discover_workflows(&options(
            Some(unix_root.path()),
            None,
            ProbePlatform::Unix,
            &prober,
        ));
        assert_eq!(
            prober.attempts(),
            vec![
                "python3 --version".to_string(),
                "node --version".to_string()
            ],
            "first working python candidate stops the search"
        );
        assert!(result.workflows.iter().all(|w| w.runtime.available));

        let prober = FakeProber::with("python", "Python 3.13.1");
        let result = discover_workflows(&options(
            Some(unix_root.path()),
            None,
            ProbePlatform::Unix,
            &prober,
        ));
        assert_eq!(
            prober.attempts(),
            vec![
                "python3 --version".to_string(),
                "python --version".to_string(),
                "node --version".to_string(),
            ]
        );
        let python = result
            .workflows
            .iter()
            .find(|w| w.id == "a")
            .expect("python workflow");
        assert_eq!(python.runtime.executable.as_deref(), Some("python"));

        let prober = FakeProber::with("python", "Python 3.13.1");
        discover_workflows(&options(
            Some(unix_root.path()),
            None,
            ProbePlatform::Windows,
            &prober,
        ));
        assert_eq!(
            prober.attempts(),
            vec![
                "py -3 --version".to_string(),
                "python --version".to_string(),
                "node --version".to_string(),
            ],
            "Windows tries the py launcher first"
        );
    }

    #[test]
    fn workflow_discovery_reports_unavailable_runtimes_with_reasons() {
        let bundled = tempfile::tempdir().expect("bundled tempdir");
        let user = tempfile::tempdir().expect("user tempdir");
        write_file(bundled.path(), "a.py", "print('a')");
        write_file(user.path(), "b.py", "print('b')");
        let prober = FakeProber::default();

        let result = discover_workflows(&options(
            Some(bundled.path()),
            Some(user.path()),
            ProbePlatform::Unix,
            &prober,
        ));

        assert_eq!(
            prober.attempts().len(),
            2,
            "python probed once despite two roots"
        );
        assert!(result.workflows.iter().all(|w| !w.runtime.available));
        let reason = result.workflows[0]
            .runtime
            .unavailable_reason
            .as_deref()
            .expect("reason present");
        assert!(reason.contains("python3"), "{reason}");
        assert!(reason.contains("python"), "{reason}");
        assert!(
            diagnostic_codes(&result).contains(&"workflow.discovery.runtime.unavailable"),
            "{:?}",
            result.diagnostics
        );
    }

    /// The packaged examples under `src-tauri/resources/workflows` ship with the
    /// application bundle and must be discovered exactly as bundled entries
    /// (rapidraw-060.7): `example_receipt.py` relies on direct-file defaults
    /// only, `example_post_image.py` carries a phase-only sidecar, and
    /// `example_post_batch.js` demonstrates a full metadata sidecar.
    #[test]
    fn workflow_discovery_finds_the_packaged_bundled_examples() {
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/workflows");
        let prober = FakeProber::default();
        {
            let mut available = prober.available.lock().unwrap();
            available.insert("python3".to_string(), "Python 3.13.0".to_string());
            available.insert("node".to_string(), "v22.1.0".to_string());
        }

        let result =
            discover_workflows(&options(Some(&bundled), None, ProbePlatform::Unix, &prober));

        // The unresolved user root is expected here; the packaged bundled root
        // itself must ship without diagnostics.
        assert_eq!(
            diagnostic_codes(&result),
            vec!["workflow.discovery.root.missing"]
        );
        assert!(
            result.diagnostics[0].message.contains("user"),
            "{:?}",
            result.diagnostics
        );
        // Sorted by (order, id): both direct-default examples keep order 100,
        // the js sidecar declares 200.
        let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "example-post-image",
                "example-receipt",
                "example-post-batch"
            ]
        );
        assert!(
            result
                .workflows
                .iter()
                .all(|w| w.source == WorkflowSource::Bundled
                    && w.selectable
                    && w.runtime.available
                    && w.diagnostics.is_empty())
        );

        // Phase-only sidecar: everything except the phase keeps its derived
        // default.
        let post_image = &result.workflows[0];
        assert_eq!(post_image.language, WorkflowLanguage::Python);
        assert_eq!(post_image.phase, WorkflowPhase::PostImage);
        assert!(post_image.script_path.ends_with("example_post_image.py"));
        assert_eq!(post_image.display_name, "example post image");
        assert_eq!(post_image.description, None);
        assert_eq!(post_image.order, WORKFLOW_DEFAULT_ORDER);
        assert_eq!(post_image.timeout_seconds, WORKFLOW_DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(post_image.on_error, WorkflowErrorPolicy::Warn);

        // Bare direct file: pure derived defaults, including the postBatch
        // phase default.
        let direct = &result.workflows[1];
        assert_eq!(direct.language, WorkflowLanguage::Python);
        assert_eq!(direct.phase, WorkflowPhase::PostBatch);
        assert!(direct.script_path.ends_with("example_receipt.py"));
        assert_eq!(direct.display_name, "example receipt");
        assert_eq!(direct.description, None);
        assert_eq!(direct.order, WORKFLOW_DEFAULT_ORDER);
        assert_eq!(direct.timeout_seconds, WORKFLOW_DEFAULT_TIMEOUT_SECONDS);
        assert_eq!(direct.on_error, WorkflowErrorPolicy::Warn);

        // Full metadata sidecar.
        let node = &result.workflows[2];
        assert_eq!(node.language, WorkflowLanguage::JavaScript);
        assert_eq!(node.phase, WorkflowPhase::PostBatch);
        assert!(node.script_path.ends_with("example_post_batch.js"));
        assert_eq!(node.display_name, "Example batch receipt");
        assert_eq!(
            node.description.as_deref(),
            Some("Writes a JSON receipt listing every exported file into the export workspace.")
        );
        assert_eq!(node.order, 200);
        assert_eq!(node.timeout_seconds, 30);
        assert_eq!(node.on_error, WorkflowErrorPolicy::Warn);
    }

    #[test]
    fn workflow_discovery_refresh_diff_reports_additions_removals_and_availability() {
        let user = tempfile::tempdir().expect("user tempdir");
        write_file(user.path(), "a.py", "print('a')");
        write_file(user.path(), "b.py", "print('b')");
        let python = FakeProber::with("python3", "Python 3.12.0");
        let opts = options(None, Some(user.path()), ProbePlatform::Unix, &python);

        let first = discover_workflows(&opts);
        let identical = discover_workflows(&opts);
        assert_eq!(
            first, identical,
            "discovery must be deterministic for unchanged inputs"
        );

        std::fs::remove_file(user.path().join("b.py")).expect("remove b.py");
        write_file(user.path(), "c.js", "console.log('c')");
        let node_only = FakeProber::with("node", "v22.1.0");
        let second = discover_workflows(&options(
            None,
            Some(user.path()),
            ProbePlatform::Unix,
            &node_only,
        ));

        let delta = diff_workflow_discovery(&first, &second);
        assert_eq!(delta.added_workflow_ids, vec!["c".to_string()]);
        assert_eq!(delta.removed_workflow_ids, vec!["b".to_string()]);
        assert_eq!(delta.availability_changes, vec!["a".to_string()]);
    }

    #[test]
    fn workflow_discovery_command_prober_bounds_missing_slow_and_fake_interpreters() {
        let prober = CommandRuntimeProber {
            timeout: Duration::from_millis(300),
        };

        let missing = prober
            .probe(&RuntimeCandidate {
                program: "rapidraw-definitely-missing-interpreter".to_string(),
                args: vec!["--version".to_string()],
            })
            .expect_err("missing program must fail");
        assert!(missing.contains("could not launch"), "{missing}");

        #[cfg(unix)]
        let hanging = RuntimeCandidate {
            program: "sleep".to_string(),
            args: vec!["5".to_string()],
        };
        #[cfg(windows)]
        let hanging = RuntimeCandidate {
            program: "ping".to_string(),
            args: vec!["-n".to_string(), "5".to_string(), "127.0.0.1".to_string()],
        };
        let slow = prober
            .probe(&hanging)
            .expect_err("hanging program must time out");
        assert!(slow.contains("did not respond"), "{slow}");

        #[cfg(unix)]
        {
            let fake_dir = tempfile::tempdir().expect("fake interpreter dir");
            let fake = fake_dir.path().join("fake-python3");
            std::fs::write(&fake, "#!/bin/sh\necho 'FakePy 9.9.9'\n").expect("write fake");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let version = prober
                .probe(&RuntimeCandidate {
                    program: fake.to_string_lossy().to_string(),
                    args: vec!["--version".to_string()],
                })
                .expect("fake interpreter responds");
            assert_eq!(version, "FakePy 9.9.9");
        }
        #[cfg(windows)]
        {
            // cmd.exe stands in for a real interpreter executable: the prober only
            // needs any spawnable program that prints a version line and exits 0. The
            // version argument avoids spaces because CreateProcess quotes such args.
            let version = prober
                .probe(&RuntimeCandidate {
                    program: "cmd".to_string(),
                    args: vec![
                        "/c".to_string(),
                        "echo".to_string(),
                        "FakePy_9.9.9".to_string(),
                    ],
                })
                .expect("fake interpreter responds");
            assert_eq!(version, "FakePy_9.9.9");
        }
    }

    #[test]
    fn workflow_discovery_containment_rejects_sibling_prefix_paths() {
        assert!(path_is_within(
            Path::new("/roots/workflows/a.py"),
            Path::new("/roots/workflows")
        ));
        assert!(path_is_within(
            Path::new(r"\\?\C:\roots\workflows\a.py"),
            Path::new(r"\\?\C:\roots\workflows")
        ));
        assert!(!path_is_within(
            Path::new("/roots/workflows-2/a.py"),
            Path::new("/roots/workflows")
        ));
        assert!(!path_is_within(
            Path::new("/elsewhere/a.py"),
            Path::new("/roots/workflows")
        ));
    }
}

#[cfg(test)]
mod runner_tests {
    use super::*;

    #[derive(Default)]
    struct RecordingProber {
        available: Mutex<HashSet<String>>,
        attempts: Mutex<Vec<String>>,
    }

    impl RecordingProber {
        fn with(program: &str) -> Self {
            let prober = Self::default();
            prober.available.lock().unwrap().insert(program.to_string());
            prober
        }

        fn attempts(&self) -> Vec<String> {
            self.attempts.lock().unwrap().clone()
        }
    }

    impl RuntimeProber for RecordingProber {
        fn probe(&self, candidate: &RuntimeCandidate) -> Result<String, String> {
            let invocation = format!("{} {}", candidate.program, candidate.args.join(" "));
            self.attempts.lock().unwrap().push(invocation);
            if self.available.lock().unwrap().contains(&candidate.program) {
                Ok("Fake 9.9.9".to_string())
            } else {
                Err(format!("{} is not available", candidate.program))
            }
        }
    }

    #[test]
    fn workflow_runner_interpreter_candidates_are_ordered_per_platform() {
        let windows =
            workflow_interpreter_candidates(WorkflowLanguage::Python, ProbePlatform::Windows);
        assert_eq!(
            windows,
            vec![
                WorkflowInterpreter {
                    language: WorkflowLanguage::Python,
                    program: "py".to_string(),
                    prefix_args: vec!["-3".to_string()],
                },
                WorkflowInterpreter {
                    language: WorkflowLanguage::Python,
                    program: "python".to_string(),
                    prefix_args: Vec::new(),
                },
            ]
        );
        let unix = workflow_interpreter_candidates(WorkflowLanguage::Python, ProbePlatform::Unix);
        assert_eq!(
            unix.iter().map(|i| i.program.as_str()).collect::<Vec<_>>(),
            vec!["python3", "python"]
        );
        assert!(unix.iter().all(|i| i.prefix_args.is_empty()));

        let node =
            workflow_interpreter_candidates(WorkflowLanguage::JavaScript, ProbePlatform::Windows);
        assert_eq!(
            node,
            vec![WorkflowInterpreter {
                language: WorkflowLanguage::JavaScript,
                program: "node".to_string(),
                prefix_args: Vec::new(),
            }]
        );
    }

    #[test]
    fn workflow_runner_resolve_returns_first_probeable_candidate_with_argument_arrays() {
        let prober = RecordingProber::with("py");
        let interpreter =
            resolve_workflow_interpreter(WorkflowLanguage::Python, ProbePlatform::Windows, &prober)
                .expect("py launcher resolves");
        assert_eq!(interpreter.program, "py");
        assert_eq!(interpreter.prefix_args, vec!["-3".to_string()]);
        assert_eq!(
            prober.attempts(),
            vec!["py -3 --version".to_string()],
            "probes append --version to the execution candidate arguments"
        );

        let prober = RecordingProber::with("python");
        let interpreter =
            resolve_workflow_interpreter(WorkflowLanguage::Python, ProbePlatform::Unix, &prober)
                .expect("fallback candidate resolves");
        assert_eq!(interpreter.program, "python");
        assert_eq!(
            prober.attempts(),
            vec![
                "python3 --version".to_string(),
                "python --version".to_string(),
            ]
        );
    }

    #[test]
    fn workflow_runner_resolve_reports_every_unavailable_candidate() {
        let prober = RecordingProber::default();
        let error = resolve_workflow_interpreter(
            WorkflowLanguage::JavaScript,
            ProbePlatform::Unix,
            &prober,
        )
        .expect_err("node is unavailable");
        match error {
            WorkflowExecutionError::MissingRuntime { ref reason, .. } => {
                assert!(reason.contains("'node --version'"), "{reason}");
                assert_eq!(error.status(), WorkflowRunStatus::Failed);
                assert_eq!(
                    error,
                    WorkflowExecutionError::MissingRuntime {
                        language: WorkflowLanguage::JavaScript,
                        reason: reason.clone(),
                    }
                );
            }
            other => panic!("expected MissingRuntime, got {other:?}"),
        }
    }

    #[test]
    fn workflow_runner_spec_rejects_oversize_requests_before_spawning() {
        let interpreter = WorkflowInterpreter {
            language: WorkflowLanguage::Python,
            program: "python3".to_string(),
            prefix_args: Vec::new(),
        };
        let script = Path::new("/workflows/big.py");
        let workspace = Path::new("/tmp/workspace");
        let roots = [workspace.to_path_buf()];
        let mut request = WorkflowRequest {
            protocol_version: WORKFLOW_PROTOCOL_VERSION,
            run_id: "run-big".to_string(),
            workflow_id: "big".to_string(),
            phase: WorkflowPhase::PostBatch,
            source_path: None,
            exported_path: None,
            artifacts: Vec::new(),
            selected_items: Vec::new(),
            exported_items: Vec::new(),
            export_settings: WorkflowExportSettings {
                file_format: "jpeg".to_string(),
                jpeg_quality: 90,
                keep_metadata: true,
                strip_gps: false,
            },
            index: None,
            total: 0,
            workspace_temp_directory: workspace.to_string_lossy().to_string(),
        };
        let padding = "p".repeat(512);
        request.selected_items = (0..4000)
            .map(|i| WorkflowItem {
                source_path: format!("{padding}-{i}.nef"),
                exported_path: None,
                artifacts: Vec::new(),
                error: None,
            })
            .collect();

        let error = WorkflowRunSpec::new(&interpreter, script, &request, workspace, &roots)
            .expect_err("oversize request must be rejected");
        assert!(error.contains("byte protocol bound"), "{error}");
        assert!(error.contains("1048576"), "{error}");
    }

    #[test]
    fn workflow_runner_canonicalize_labels_outside_roots_external() {
        let base = tempfile::tempdir().expect("base tempdir");
        let workspace = base.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let export_root = base.path().join("export");
        std::fs::create_dir_all(&export_root).expect("export root");
        std::fs::write(workspace.join("receipt.json"), "{}").expect("receipt");
        std::fs::write(export_root.join("side.txt"), "{}").expect("side");

        let outside = if cfg!(windows) {
            r"C:\Windows\win.ini".to_string()
        } else {
            "/etc/hostname".to_string()
        };
        let paths = vec![
            "receipt.json".to_string(),
            export_root.join("side.txt").to_string_lossy().to_string(),
            "../escape.txt".to_string(),
            outside,
            "missing.json".to_string(),
        ];
        let artifacts =
            canonicalize_workflow_artifacts(&paths, &workspace, std::slice::from_ref(&export_root));
        let kinds: Vec<Option<&str>> = artifacts.iter().map(|a| a.kind.as_deref()).collect();
        assert_eq!(
            kinds,
            vec![
                None,
                None,
                Some("external"),
                Some("external"),
                Some("external")
            ]
        );
        let canonical_workspace =
            strip_verbatim(&workspace.canonicalize().expect("canonical workspace"));
        assert_eq!(
            Path::new(&artifacts[0].path),
            canonical_workspace.join("receipt.json")
        );
        assert!(artifacts[4].path.ends_with("missing.json"));
    }

    #[test]
    fn workflow_runner_stderr_text_is_bounded_and_marked_when_truncated() {
        let plain = stderr_display_text(b"all good", false);
        assert_eq!(plain, "all good");

        let truncated = stderr_display_text(b"x", true);
        assert!(
            truncated.ends_with(WORKFLOW_STDERR_TRUNCATION_MARKER),
            "{truncated}"
        );
    }

    #[test]
    fn workflow_runner_malformed_reason_carries_a_bounded_stderr_excerpt() {
        let error = serde_json::from_str::<WorkflowResponse>("nope").expect_err("invalid json");
        let reason = malformed_output_reason(error, "");
        assert!(
            reason.contains("not a valid protocol v1 JSON document"),
            "{reason}"
        );

        let error = serde_json::from_str::<WorkflowResponse>("nope").expect_err("invalid json");
        let long_stderr = "e".repeat(10_000);
        let reason = malformed_output_reason(error, &long_stderr);
        assert!(reason.contains("stderr: "), "{reason}");
        assert!(
            reason.chars().count() < long_stderr.chars().count(),
            "excerpt must stay small: {} chars",
            reason.chars().count()
        );
    }

    #[test]
    fn workflow_runner_concurrency_gate_bounds_permits() {
        let gate = WorkflowConcurrencyGate::new(WORKFLOW_MAX_CONCURRENT_RUNS);
        assert_eq!(gate.limit(), WORKFLOW_MAX_CONCURRENT_RUNS);

        let mut permits = Vec::new();
        for _ in 0..WORKFLOW_MAX_CONCURRENT_RUNS {
            permits.push(gate.try_acquire().expect("permit within limit"));
        }
        assert!(gate.try_acquire().is_none(), "limit enforced");
        permits.pop();
        assert!(gate.try_acquire().is_some(), "released permit reusable");
    }

    #[test]
    fn workflow_runner_limits_default_to_finite_protocol_bounds() {
        let limits = WorkflowRunLimits::default();
        assert_eq!(
            limits.timeout,
            Duration::from_secs(WORKFLOW_DEFAULT_TIMEOUT_SECONDS)
        );
        assert!(!limits.timeout.is_zero());
        assert_eq!(limits.max_stdout_bytes, WORKFLOW_MAX_RESPONSE_BYTES);
        assert_eq!(limits.max_stderr_bytes, WORKFLOW_MAX_STDERR_BYTES);
        assert!(WORKFLOW_MAX_RESPONSE_BYTES.is_multiple_of(1024));
        assert!(WORKFLOW_MAX_STDERR_BYTES.is_multiple_of(1024));
    }

    #[test]
    fn workflow_runner_minimal_environment_copies_only_allowlisted_names() {
        let environment = minimal_workflow_environment();
        #[cfg(windows)]
        let allowlist = [
            "PATH",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "SYSTEMROOT",
            "COMSPEC",
        ];
        #[cfg(not(windows))]
        let allowlist = ["PATH", "TEMP", "TMP", "HOME"];
        for (name, value) in &environment {
            assert!(allowlist.contains(&name.as_str()), "{name} forwarded");
            assert!(!value.is_empty(), "{name} must carry a real value");
        }
    }
}
