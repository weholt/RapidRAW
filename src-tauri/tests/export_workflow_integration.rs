//! Integration tests for wiring workflow phases into the export pipeline
//! (`rapidraw-060.4`).
//!
//! These tests drive the export integration service — selection resolution,
//! the per-image postImage sequencing, the once-per-batch postBatch
//! invocation, error policies, cancellation, and workspace lifecycle — through
//! an injected recording runner so no interpreter is required. The engine is
//! the same service `export_images_impl` uses for GUI and headless exports;
//! marker workflows prove phase inputs, ordering, and item bookkeeping.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rapidraw_lib::export_workflows::{
    DiscoveredWorkflow, ExportItemOutcome, ExportWorkflowEngine, WORKFLOW_PROTOCOL_VERSION,
    WorkflowArtifact, WorkflowConcurrencyGate, WorkflowDiagnostic, WorkflowDiscoveryResult,
    WorkflowErrorPolicy, WorkflowExecutionError, WorkflowExportSettings, WorkflowItem,
    WorkflowLanguage, WorkflowPhase, WorkflowPolicyFailure, WorkflowProgressEvent,
    WorkflowProgressPhase, WorkflowProgressSink, WorkflowRequest, WorkflowResponse,
    WorkflowRunOutput, WorkflowRunResult, WorkflowRunSpec, WorkflowRunStatus, WorkflowRunner,
    WorkflowRuntime, WorkflowSelectionError, WorkflowSource, plan_export_workflows,
    summarize_export_results,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn available_runtime() -> WorkflowRuntime {
    WorkflowRuntime {
        available: true,
        executable: Some("python".to_string()),
        version: Some("Python 3.13.0".to_string()),
        unavailable_reason: None,
    }
}

/// Shared real workflow root: every discovered entry gets an actual script
/// file so the engine's spawn-time identity revalidation has a canonical
/// path inside a root to validate against, exactly like production.
fn script_root() -> PathBuf {
    static SCRIPT_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    SCRIPT_ROOT
        .get_or_init(|| {
            let root = std::env::temp_dir()
                .join("rapidraw-export-workflow-integration")
                .join(format!("scripts-{}", std::process::id()));
            std::fs::create_dir_all(&root).expect("create script root");
            root.canonicalize().expect("canonicalize script root")
        })
        .clone()
}

fn discovered(
    id: &str,
    phase: WorkflowPhase,
    order: i32,
    on_error: WorkflowErrorPolicy,
) -> DiscoveredWorkflow {
    let script = script_root().join(format!("{id}.py"));
    std::fs::write(&script, format!("print('{id}')\n")).expect("write workflow script");
    DiscoveredWorkflow {
        id: id.to_string(),
        display_name: id.to_string().replace('-', " "),
        description: None,
        language: WorkflowLanguage::Python,
        phase,
        order,
        timeout_seconds: 60,
        on_error,
        source: WorkflowSource::Bundled,
        script_path: script.to_string_lossy().to_string(),
        selectable: true,
        runtime: available_runtime(),
        diagnostics: Vec::<WorkflowDiagnostic>::new(),
    }
}

fn discovery_from(workflows: Vec<DiscoveredWorkflow>) -> WorkflowDiscoveryResult {
    WorkflowDiscoveryResult {
        workflows,
        diagnostics: Vec::new(),
    }
}

fn selection(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

fn export_settings() -> WorkflowExportSettings {
    WorkflowExportSettings {
        file_format: "jpeg".to_string(),
        jpeg_quality: 92,
        keep_metadata: true,
        strip_gps: false,
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir()
        .join("rapidraw-export-workflow-integration")
        .join(format!(
            "{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clean stale test dir");
    }
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

/// Interpreter probe that always succeeds, keeping the engine deterministic
/// and independent of the host's installed runtimes.
struct FakeProber;

impl rapidraw_lib::export_workflows::RuntimeProber for FakeProber {
    fn probe(
        &self,
        _candidate: &rapidraw_lib::export_workflows::RuntimeCandidate,
    ) -> Result<String, String> {
        Ok("Fake Runtime 1.0".to_string())
    }
}

/// One scripted behavior for the recording runner.
#[derive(Clone)]
enum Behavior {
    Respond(WorkflowResponse),
    RespondWithStderr(WorkflowResponse, String),
    Fail(WorkflowExecutionError),
}

#[derive(Debug, Clone)]
struct RecordedCall {
    workflow_id: String,
    phase: WorkflowPhase,
    protocol_version: u32,
    run_id: String,
    source_path: Option<String>,
    exported_path: Option<String>,
    artifacts_in: Vec<WorkflowArtifact>,
    selected_items: Vec<WorkflowItem>,
    exported_items: Vec<WorkflowItem>,
    index: Option<u64>,
    total: u64,
    workspace_temp_directory: String,
    export_settings: WorkflowExportSettings,
    timeout_seconds: u64,
    exported_path_exists: bool,
    artifacts_exist_on_disk: bool,
}

/// Injected runner capturing every invocation and replaying scripted
/// behaviors by workflow id.
struct RecordingRunner {
    calls: Mutex<Vec<RecordedCall>>,
    behaviors: Mutex<std::collections::HashMap<String, Behavior>>,
}

impl RecordingRunner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            behaviors: Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn respond(&self, workflow_id: &str, response: WorkflowResponse) {
        self.behaviors
            .lock()
            .unwrap()
            .insert(workflow_id.to_string(), Behavior::Respond(response));
    }

    /// Like [`Self::respond`] but the run output carries diagnostic stderr text.
    fn respond_with_stderr(&self, workflow_id: &str, response: WorkflowResponse, stderr: String) {
        self.behaviors.lock().unwrap().insert(
            workflow_id.to_string(),
            Behavior::RespondWithStderr(response, stderr),
        );
    }

    fn fail(&self, workflow_id: &str, error: WorkflowExecutionError) {
        self.behaviors
            .lock()
            .unwrap()
            .insert(workflow_id.to_string(), Behavior::Fail(error));
    }

    fn calls_for(&self, workflow_id: &str) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.workflow_id == workflow_id)
            .cloned()
            .collect()
    }

    fn sequence(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.workflow_id.clone())
            .collect()
    }
}

impl WorkflowRunner for RecordingRunner {
    fn run(
        &self,
        spec: &WorkflowRunSpec<'_>,
        _cancel: &AtomicBool,
    ) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
        let request: WorkflowRequest =
            serde_json::from_str(&spec.request_json).expect("spec carries a valid request");
        let behavior = self
            .behaviors
            .lock()
            .unwrap()
            .get(&request.workflow_id)
            .cloned()
            .expect("behavior scripted for invoked workflow");
        self.calls.lock().unwrap().push(RecordedCall {
            workflow_id: request.workflow_id.clone(),
            phase: request.phase,
            protocol_version: request.protocol_version,
            run_id: request.run_id.clone(),
            source_path: request.source_path.clone(),
            exported_path: request.exported_path.clone(),
            artifacts_in: request.artifacts.clone(),
            selected_items: request.selected_items.clone(),
            exported_items: request.exported_items.clone(),
            index: request.index,
            total: request.total,
            workspace_temp_directory: request.workspace_temp_directory.clone(),
            export_settings: request.export_settings.clone(),
            timeout_seconds: spec.limits.timeout.as_secs(),
            exported_path_exists: request
                .exported_path
                .as_deref()
                .map(|path| Path::new(path).is_file())
                .unwrap_or(false),
            artifacts_exist_on_disk: request
                .artifacts
                .iter()
                .all(|artifact| Path::new(&artifact.path).is_file()),
        });
        match behavior {
            Behavior::Respond(response) => Ok(WorkflowRunOutput {
                produced_artifacts: response
                    .produced_artifact_paths
                    .iter()
                    .map(|path| WorkflowArtifact {
                        path: path.clone(),
                        kind: None,
                    })
                    .collect(),
                response,
                stderr: String::new(),
            }),
            Behavior::RespondWithStderr(response, stderr) => Ok(WorkflowRunOutput {
                produced_artifacts: response
                    .produced_artifact_paths
                    .iter()
                    .map(|path| WorkflowArtifact {
                        path: path.clone(),
                        kind: None,
                    })
                    .collect(),
                response,
                stderr,
            }),
            Behavior::Fail(error) => Err(error),
        }
    }
}

fn ok_response(message: &str) -> WorkflowResponse {
    WorkflowResponse {
        ok: true,
        message: Some(message.to_string()),
        warnings: Vec::new(),
        produced_artifact_paths: Vec::new(),
    }
}

fn script_failure(message: &str) -> WorkflowResponse {
    WorkflowResponse {
        ok: false,
        message: Some(message.to_string()),
        warnings: Vec::new(),
        produced_artifact_paths: Vec::new(),
    }
}

fn marker_workflow(
    id: &str,
    phase: WorkflowPhase,
    on_error: WorkflowErrorPolicy,
) -> DiscoveredWorkflow {
    discovered(id, phase, 100, on_error)
}

fn engine_with(
    runner: &Arc<RecordingRunner>,
    workflows: Vec<DiscoveredWorkflow>,
    total: u64,
    workspace_parent: &Path,
) -> ExportWorkflowEngine {
    engine_with_progress(runner, workflows, total, workspace_parent, None)
}

/// [`ExportWorkflowEngine::new`] wired with an optional progress sink so event
/// sequences can be recorded.
fn engine_with_progress(
    runner: &Arc<RecordingRunner>,
    workflows: Vec<DiscoveredWorkflow>,
    total: u64,
    workspace_parent: &Path,
    progress: Option<WorkflowProgressSink>,
) -> ExportWorkflowEngine {
    let ids = workflows
        .iter()
        .map(|workflow| workflow.id.clone())
        .collect::<Vec<_>>();
    let plan = plan_export_workflows(&ids, &discovery_from(workflows))
        .expect("plan resolves")
        .expect("plan is non-empty");
    ExportWorkflowEngine::new(
        plan,
        runner.clone(),
        Arc::new(FakeProber),
        WorkflowConcurrencyGate::default(),
        export_settings(),
        workspace_parent.to_path_buf(),
        vec![script_root()],
        total,
        progress,
    )
}

fn write_export_output(dir: &Path, name: &str) -> (String, Vec<WorkflowArtifact>) {
    let exported = dir.join(name);
    std::fs::write(&exported, b"jpeg-bytes").expect("write exported file");
    let mask = dir.join(format!("{name}.mask.png"));
    std::fs::write(&mask, b"mask-bytes").expect("write mask artifact");
    (
        exported.to_string_lossy().to_string(),
        vec![WorkflowArtifact {
            path: mask.to_string_lossy().to_string(),
            kind: Some("mask".to_string()),
        }],
    )
}

// ---------------------------------------------------------------------------
// selection resolution
// ---------------------------------------------------------------------------

#[test]
fn export_workflow_integration_empty_selection_yields_no_plan() {
    let result = plan_export_workflows(
        &[],
        &discovery_from(vec![discovered(
            "bundled-only",
            WorkflowPhase::PostBatch,
            100,
            WorkflowErrorPolicy::Warn,
        )]),
    );
    assert_eq!(result, Ok(None));
}

#[test]
fn export_workflow_integration_resolution_preserves_user_order_and_splits_phases() {
    let registry = vec![
        discovered(
            "alpha-postbatch",
            WorkflowPhase::PostBatch,
            1,
            WorkflowErrorPolicy::Warn,
        ),
        discovered(
            "beta-postimage",
            WorkflowPhase::PostImage,
            2,
            WorkflowErrorPolicy::Warn,
        ),
        discovered(
            "gamma-postimage",
            WorkflowPhase::PostImage,
            3,
            WorkflowErrorPolicy::Fail,
        ),
    ];
    // User selection deliberately differs from registry order (order, id).
    let plan = plan_export_workflows(
        &selection(&["gamma-postimage", "alpha-postbatch", "beta-postimage"]),
        &discovery_from(registry),
    )
    .expect("selection resolves")
    .expect("plan exists");

    let post_image_ids: Vec<&str> = plan
        .post_image
        .iter()
        .map(|workflow| workflow.id.as_str())
        .collect();
    let post_batch_ids: Vec<&str> = plan
        .post_batch
        .iter()
        .map(|workflow| workflow.id.as_str())
        .collect();
    assert_eq!(post_image_ids, vec!["gamma-postimage", "beta-postimage"]);
    assert_eq!(post_batch_ids, vec!["alpha-postbatch"]);
    assert!(!plan.run_id.is_empty());
}

#[test]
fn export_workflow_integration_resolution_rejects_unknown_ids() {
    let error = plan_export_workflows(
        &selection(&["known", "missing"]),
        &discovery_from(vec![discovered(
            "known",
            WorkflowPhase::PostImage,
            100,
            WorkflowErrorPolicy::Warn,
        )]),
    )
    .unwrap_err();
    assert_eq!(
        error,
        WorkflowSelectionError::UnknownWorkflow {
            id: "missing".to_string()
        }
    );
}

#[test]
fn export_workflow_integration_resolution_rejects_unselectable_and_unavailable_entries() {
    let mut unselectable = discovered(
        "bad-metadata",
        WorkflowPhase::PostImage,
        100,
        WorkflowErrorPolicy::Warn,
    );
    unselectable.selectable = false;
    let unavailable = DiscoveredWorkflow {
        runtime: WorkflowRuntime {
            available: false,
            executable: None,
            version: None,
            unavailable_reason: Some("no python runtime".to_string()),
        },
        ..discovered(
            "no-runtime",
            WorkflowPhase::PostImage,
            100,
            WorkflowErrorPolicy::Warn,
        )
    };

    let error = plan_export_workflows(
        &selection(&["bad-metadata"]),
        &discovery_from(vec![unselectable]),
    )
    .unwrap_err();
    assert_eq!(
        error,
        WorkflowSelectionError::UnselectableWorkflow {
            id: "bad-metadata".to_string()
        }
    );

    let error = plan_export_workflows(
        &selection(&["no-runtime"]),
        &discovery_from(vec![unavailable]),
    )
    .unwrap_err();
    assert_eq!(
        error,
        WorkflowSelectionError::UnavailableRuntime {
            id: "no-runtime".to_string(),
            reason: "no python runtime".to_string(),
        }
    );
}

#[test]
fn export_workflow_integration_resolution_rejects_duplicate_selection() {
    let error = plan_export_workflows(
        &selection(&["dup", "dup"]),
        &discovery_from(vec![discovered(
            "dup",
            WorkflowPhase::PostImage,
            100,
            WorkflowErrorPolicy::Warn,
        )]),
    )
    .unwrap_err();
    assert_eq!(
        error,
        WorkflowSelectionError::DuplicateSelection {
            id: "dup".to_string()
        }
    );
}

// ---------------------------------------------------------------------------
// postImage phase: timing, ordering, chaining
// ---------------------------------------------------------------------------

#[test]
fn export_workflow_integration_post_image_runs_in_user_order_after_outputs_exist() {
    let dir = temp_dir("post-image-order");
    let runner = RecordingRunner::new();
    runner.respond("second", ok_response("second done"));
    runner.respond("first", ok_response("first done"));
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow("first", WorkflowPhase::PostImage, WorkflowErrorPolicy::Warn),
            marker_workflow(
                "second",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
        ],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0001.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let results = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0001.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("postImage workflows succeed");

    // Sequential per-image order: user order first, then second.
    assert_eq!(runner.sequence(), vec!["first", "second"]);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].status, WorkflowRunStatus::Succeeded);

    let first = &runner.calls_for("first")[0];
    let second = &runner.calls_for("second")[0];
    for call in [&first, &second] {
        assert_eq!(call.phase, WorkflowPhase::PostImage);
        assert_eq!(call.run_id, engine.run_id());
        assert_eq!(call.source_path.as_deref(), Some("raw/DSC_0001.NEF"));
        assert_eq!(call.exported_path.as_deref(), Some(exported.as_str()));
        assert_eq!(call.index, Some(0));
        assert_eq!(call.total, 1);
        assert_eq!(call.export_settings, export_settings());
        // postImage requests never carry batch item lists.
        assert!(call.selected_items.is_empty());
        assert!(call.exported_items.is_empty());
        // Fully written outputs: the exported file and mask artifacts exist.
        assert!(call.exported_path_exists);
        assert!(call.artifacts_exist_on_disk);
        assert!(Path::new(&call.workspace_temp_directory).is_dir());
    }
    // The initial request carries the mask artifact...
    assert_eq!(first.artifacts_in.len(), 1);
}

#[test]
fn export_workflow_integration_post_image_chains_produced_artifacts_in_order() {
    let dir = temp_dir("post-image-chain");
    let runner = RecordingRunner::new();
    let mut receipt = ok_response("wrote receipt");
    receipt.produced_artifact_paths = vec!["receipt.json".to_string()];
    runner.respond("producer", receipt);
    runner.respond("consumer", ok_response("consumer done"));
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow(
                "producer",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
            marker_workflow(
                "consumer",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
        ],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0002.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let results = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0002.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("chained workflows succeed");

    let consumer = &runner.calls_for("consumer")[0];
    // The consumer receives the mask artifact plus the producer's artifact.
    assert_eq!(consumer.artifacts_in.len(), 2);
    assert_eq!(results[0].produced_artifacts.len(), 1);
    // The item's artifact list is extended for downstream records.
    assert_eq!(item_artifacts.len(), 2);
}

#[test]
fn export_workflow_integration_post_image_uses_configured_timeout_and_same_run_id() {
    let dir = temp_dir("post-image-timeout");
    let runner = RecordingRunner::new();
    runner.respond("slow", ok_response("slow done"));
    let mut slow = marker_workflow("slow", WorkflowPhase::PostImage, WorkflowErrorPolicy::Warn);
    slow.timeout_seconds = 123;
    let engine = engine_with(&runner, vec![slow], 3, &dir);

    let (exported, artifacts) = write_export_output(&dir, "DSC_0003.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    engine
        .run_post_image(
            &cancel,
            "raw/DSC_0003.NEF",
            &exported,
            &mut item_artifacts,
            2,
            &dir,
        )
        .expect("workflow succeeds");

    let call = &runner.calls_for("slow")[0];
    assert_eq!(call.timeout_seconds, 123);
    assert_eq!(call.index, Some(2));
    assert_eq!(call.total, 3);

    // The same run id is reused for a second image in the batch.
    let (exported_b, artifacts_b) = write_export_output(&dir, "DSC_0004.jpg");
    let mut item_artifacts_b = artifacts_b;
    engine
        .run_post_image(
            &cancel,
            "raw/DSC_0004.NEF",
            &exported_b,
            &mut item_artifacts_b,
            1,
            &dir,
        )
        .expect("second image workflows succeed");
    assert_eq!(runner.calls_for("slow")[1].run_id, engine.run_id());
}

#[test]
fn export_workflow_integration_post_image_skipped_entirely_for_images_without_workflows() {
    let dir = temp_dir("post-image-empty");
    let runner = RecordingRunner::new();
    // An engine with no postImage workflows runs nothing and succeeds.
    let engine = ExportWorkflowEngine::new(
        plan_export_workflows(
            &selection(&["only-postbatch"]),
            &discovery_from(vec![marker_workflow(
                "only-postbatch",
                WorkflowPhase::PostBatch,
                WorkflowErrorPolicy::Warn,
            )]),
        )
        .expect("resolves")
        .expect("some"),
        runner.clone(),
        Arc::new(FakeProber),
        WorkflowConcurrencyGate::default(),
        export_settings(),
        dir,
        vec![script_root()],
        1,
        None,
    );
    assert!(!engine.has_post_image());
    assert!(engine.has_post_batch());

    let cancel = AtomicBool::new(false);
    let mut artifacts = Vec::new();
    let results = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0005.NEF",
            "out/DSC_0005.jpg",
            &mut artifacts,
            0,
            Path::new("out"),
        )
        .expect("no-op succeeds");
    assert!(results.is_empty());
    assert!(runner.calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// error policies
// ---------------------------------------------------------------------------

#[test]
fn export_workflow_integration_warn_policy_records_warning_and_continues() {
    let dir = temp_dir("policy-warn");
    let runner = RecordingRunner::new();
    runner.respond("flaky", script_failure("archive service offline"));
    runner.respond("next", ok_response("next done"));
    let mut flaky = marker_workflow("flaky", WorkflowPhase::PostImage, WorkflowErrorPolicy::Warn);
    flaky.timeout_seconds = 30;
    let engine = engine_with(
        &runner,
        vec![
            flaky,
            marker_workflow("next", WorkflowPhase::PostImage, WorkflowErrorPolicy::Warn),
        ],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0006.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let results = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0006.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("warn policy never fails the item");

    assert_eq!(runner.sequence(), vec!["flaky", "next"]);
    assert_eq!(results[0].status, WorkflowRunStatus::Failed);
    assert_eq!(
        results[0].message.as_deref(),
        Some("archive service offline")
    );
}

#[test]
fn export_workflow_integration_fail_policy_fails_item_with_typed_reason() {
    let dir = temp_dir("policy-fail");
    let runner = RecordingRunner::new();
    runner.respond("guard", script_failure("validation failed"));
    runner.respond("after", ok_response("unreached"));
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow("guard", WorkflowPhase::PostImage, WorkflowErrorPolicy::Fail),
            marker_workflow("after", WorkflowPhase::PostImage, WorkflowErrorPolicy::Fail),
        ],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0007.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let error = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0007.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Failed(
            WorkflowPolicyFailure::ScriptReported {
                workflow_id: "guard".to_string(),
                message: "validation failed".to_string(),
            }
        )
    );
    // A fail-policy failure stops the remaining workflows for that image.
    assert_eq!(runner.sequence(), vec!["guard"]);
}

#[test]
fn export_workflow_integration_execution_errors_follow_the_policy() {
    let dir = temp_dir("policy-execution");
    let runner = RecordingRunner::new();
    runner.fail(
        "crash-warn",
        WorkflowExecutionError::NonZeroExit { code: 3 },
    );
    runner.fail("crash-fail", WorkflowExecutionError::TimedOut);
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow(
                "crash-warn",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
            marker_workflow(
                "crash-fail",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Fail,
            ),
        ],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0008.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let error = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0008.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Failed(
            WorkflowPolicyFailure::ExecutionError {
                workflow_id: "crash-fail".to_string(),
                error: WorkflowExecutionError::TimedOut,
            }
        )
    );
    // The warn-policy execution failure before it was recorded, not fatal.
    assert_eq!(runner.sequence(), vec!["crash-warn", "crash-fail"]);
}

#[test]
fn export_workflow_integration_warnings_are_not_failures() {
    let dir = temp_dir("policy-warnings");
    let runner = RecordingRunner::new();
    let mut warned = ok_response("done with warnings");
    warned.warnings = vec!["artifact already existed".to_string()];
    runner.respond("warned", warned);
    let engine = engine_with(
        &runner,
        vec![marker_workflow(
            "warned",
            WorkflowPhase::PostImage,
            WorkflowErrorPolicy::Fail,
        )],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0009.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let results = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0009.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("warnings do not trip the fail policy");
    assert_eq!(results[0].status, WorkflowRunStatus::Warned);
    assert_eq!(
        results[0].warnings,
        vec!["artifact already existed".to_string()]
    );
}

// ---------------------------------------------------------------------------
// postBatch phase
// ---------------------------------------------------------------------------

fn sample_items(dir: &Path) -> (Vec<WorkflowItem>, Vec<WorkflowItem>) {
    let (exported_a, artifacts_a) = write_export_output(dir, "batch_a.jpg");
    let selected = vec![
        WorkflowItem {
            source_path: "raw/batch_a.NEF".to_string(),
            exported_path: None,
            artifacts: Vec::new(),
            error: None,
        },
        WorkflowItem {
            source_path: "raw/batch_b.NEF".to_string(),
            exported_path: None,
            artifacts: Vec::new(),
            error: None,
        },
        WorkflowItem {
            source_path: "raw/batch_c.NEF".to_string(),
            exported_path: None,
            artifacts: Vec::new(),
            error: None,
        },
    ];
    let exported_records = vec![
        WorkflowItem {
            source_path: "raw/batch_a.NEF".to_string(),
            exported_path: Some(exported_a),
            artifacts: artifacts_a,
            error: None,
        },
        WorkflowItem {
            source_path: "raw/batch_b.NEF".to_string(),
            exported_path: None,
            artifacts: Vec::new(),
            error: Some("render failed".to_string()),
        },
        WorkflowItem {
            source_path: "raw/batch_c.NEF".to_string(),
            exported_path: None,
            artifacts: Vec::new(),
            error: Some("Export cancelled".to_string()),
        },
    ];
    (selected, exported_records)
}

#[test]
fn export_workflow_integration_post_batch_runs_once_with_settled_records() {
    let dir = temp_dir("post-batch");
    let runner = RecordingRunner::new();
    runner.respond("batch-summary", ok_response("summary written"));
    let engine = engine_with(
        &runner,
        vec![marker_workflow(
            "batch-summary",
            WorkflowPhase::PostBatch,
            WorkflowErrorPolicy::Warn,
        )],
        3,
        &dir,
    );

    let (selected, exported_records) = sample_items(&dir);
    let cancel = AtomicBool::new(false);
    let results = engine
        .run_post_batch(&cancel, &selected, &exported_records, &dir)
        .expect("postBatch succeeds");

    assert_eq!(runner.sequence(), vec!["batch-summary"]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_path, None);

    let call = &runner.calls_for("batch-summary")[0];
    assert_eq!(call.phase, WorkflowPhase::PostBatch);
    assert_eq!(call.run_id, engine.run_id());
    assert_eq!(call.source_path, None);
    assert_eq!(call.exported_path, None);
    assert_eq!(call.index, None);
    assert_eq!(call.total, 3);
    assert!(call.artifacts_in.is_empty());
    // Successful, failed, and skipped items are all present.
    assert_eq!(call.selected_items.len(), 3);
    assert_eq!(call.exported_items.len(), 3);
    assert!(call.exported_items[0].exported_path.is_some());
    assert!(call.exported_items[0].error.is_none());
    assert_eq!(
        call.exported_items[1].error.as_deref(),
        Some("render failed")
    );
    assert_eq!(
        call.exported_items[2].error.as_deref(),
        Some("Export cancelled")
    );
    assert_eq!(call.protocol_version, WORKFLOW_PROTOCOL_VERSION);
}

#[test]
fn export_workflow_integration_post_batch_fail_policy_fails_the_batch() {
    let dir = temp_dir("post-batch-fail");
    let runner = RecordingRunner::new();
    runner.respond("uploader", script_failure("bucket unreachable"));
    runner.respond("second-uploader", ok_response("unreached"));
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow(
                "uploader",
                WorkflowPhase::PostBatch,
                WorkflowErrorPolicy::Fail,
            ),
            marker_workflow(
                "second-uploader",
                WorkflowPhase::PostBatch,
                WorkflowErrorPolicy::Fail,
            ),
        ],
        2,
        &dir,
    );

    let (selected, exported_records) = sample_items(&dir);
    let cancel = AtomicBool::new(false);
    let error = engine
        .run_post_batch(&cancel, &selected, &exported_records, &dir)
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Failed(
            WorkflowPolicyFailure::ScriptReported {
                workflow_id: "uploader".to_string(),
                message: "bucket unreachable".to_string(),
            }
        )
    );
    assert_eq!(runner.sequence(), vec!["uploader"]);
}

#[test]
fn export_workflow_integration_post_batch_is_skipped_when_cancelled() {
    let dir = temp_dir("post-batch-cancel");
    let runner = RecordingRunner::new();
    runner.respond("batch-summary", ok_response("unreached"));
    let engine = engine_with(
        &runner,
        vec![marker_workflow(
            "batch-summary",
            WorkflowPhase::PostBatch,
            WorkflowErrorPolicy::Warn,
        )],
        2,
        &dir,
    );

    let cancel = AtomicBool::new(true);
    let (selected, exported_records) = sample_items(&dir);
    let error = engine
        .run_post_batch(&cancel, &selected, &exported_records, &dir)
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Cancelled
    );
    assert!(runner.calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// cancellation
// ---------------------------------------------------------------------------

#[test]
fn export_workflow_integration_cancellation_skips_unstarted_workflows() {
    let dir = temp_dir("cancel-skip");
    let runner = RecordingRunner::new();
    runner.fail("terminates", WorkflowExecutionError::Cancelled);
    runner.respond("never-starts", ok_response("unreached"));
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow(
                "terminates",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
            marker_workflow(
                "never-starts",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
        ],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0010.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let error = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0010.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .unwrap_err();
    // An in-flight workflow terminated by cancellation cancels the item; the
    // unstarted workflow is skipped.
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Cancelled
    );
    assert_eq!(runner.sequence(), vec!["terminates"]);
}

#[test]
fn export_workflow_integration_pre_cancelled_run_invokes_nothing() {
    let dir = temp_dir("cancel-pre");
    let runner = RecordingRunner::new();
    runner.respond("any", ok_response("unreached"));
    let engine = engine_with(
        &runner,
        vec![marker_workflow(
            "any",
            WorkflowPhase::PostImage,
            WorkflowErrorPolicy::Warn,
        )],
        1,
        &dir,
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_0011.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(true);
    let error = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0011.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Cancelled
    );
    assert!(runner.calls.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// concurrency gate and workspace lifecycle
// ---------------------------------------------------------------------------

#[test]
fn export_workflow_integration_workflow_processes_wait_on_the_shared_gate() {
    let dir = temp_dir("gate-wait");
    let runner = RecordingRunner::new();
    runner.respond("gated", ok_response("done"));
    let gate = WorkflowConcurrencyGate::new(1);
    let plan = plan_export_workflows(
        &selection(&["gated"]),
        &discovery_from(vec![marker_workflow(
            "gated",
            WorkflowPhase::PostImage,
            WorkflowErrorPolicy::Warn,
        )]),
    )
    .expect("resolves")
    .expect("some");
    let engine = ExportWorkflowEngine::new(
        plan,
        runner.clone(),
        Arc::new(FakeProber),
        gate.clone(),
        export_settings(),
        dir.clone(),
        vec![script_root()],
        1,
        None,
    );

    // Exhaust the single shared permit; the engine must not run the workflow
    // while the gate is held and must observe cancellation instead.
    let permit = gate.try_acquire().expect("first permit acquired");
    let cancel = Arc::new(AtomicBool::new(false));
    let canceller = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::SeqCst);
        })
    };
    let (exported, artifacts) = write_export_output(&dir, "DSC_0012.jpg");
    let mut item_artifacts = artifacts;
    let error = engine
        .run_post_image(
            &cancel,
            "raw/DSC_0012.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Cancelled
    );
    assert!(runner.calls.lock().unwrap().is_empty());
    canceller.join().unwrap();
    drop(permit);
}

#[test]
fn export_workflow_integration_workspace_is_shared_per_run_and_cleaned_up() {
    let dir = temp_dir("workspace-lifecycle");
    let runner = RecordingRunner::new();
    runner.respond("post-image-wf", ok_response("done"));
    runner.respond("post-batch-wf", ok_response("done"));
    let engine = engine_with(
        &runner,
        vec![
            marker_workflow(
                "post-image-wf",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
            marker_workflow(
                "post-batch-wf",
                WorkflowPhase::PostBatch,
                WorkflowErrorPolicy::Warn,
            ),
        ],
        1,
        &dir,
    );

    // Nothing is created before the first invocation.
    assert!(!dir.join(engine.run_id()).exists());

    let (exported, artifacts) = write_export_output(&dir, "DSC_0013.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    engine
        .run_post_image(
            &cancel,
            "raw/DSC_0013.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("postImage succeeds");
    engine
        .run_post_batch(
            &cancel,
            &[WorkflowItem {
                source_path: "raw/DSC_0013.NEF".to_string(),
                exported_path: None,
                artifacts: Vec::new(),
                error: None,
            }],
            &[WorkflowItem {
                source_path: "raw/DSC_0013.NEF".to_string(),
                exported_path: Some(exported),
                artifacts: item_artifacts.clone(),
                error: None,
            }],
            &dir,
        )
        .expect("postBatch succeeds");

    let post_image_workspace = runner.calls_for("post-image-wf")[0]
        .workspace_temp_directory
        .clone();
    let post_batch_workspace = runner.calls_for("post-batch-wf")[0]
        .workspace_temp_directory
        .clone();
    assert_eq!(post_image_workspace, post_batch_workspace);
    let workspace = PathBuf::from(&post_image_workspace);
    assert!(workspace.is_dir());

    engine.cleanup();
    assert!(!workspace.exists());
}

#[test]
fn export_workflow_integration_run_ids_are_distinct_per_run() {
    let first = rapidraw_lib::export_workflows::generate_workflow_run_id();
    let second = rapidraw_lib::export_workflows::generate_workflow_run_id();
    assert_ne!(first, second);
    assert!(first.starts_with("run-"));
}

// ---------------------------------------------------------------------------
// progress events
// ---------------------------------------------------------------------------

fn recorded_events() -> (Arc<Mutex<Vec<WorkflowProgressEvent>>>, WorkflowProgressSink) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    (
        events,
        Arc::new(move |event: WorkflowProgressEvent| {
            sink_events.lock().unwrap().push(event);
        }),
    )
}

fn event_phases(events: &Arc<Mutex<Vec<WorkflowProgressEvent>>>) -> Vec<WorkflowProgressPhase> {
    events.lock().unwrap().iter().map(|e| e.phase).collect()
}

#[test]
fn export_workflow_progress_events_sequence_post_image_and_post_batch_runs() {
    let dir = temp_dir("progress-sequence");
    let runner = RecordingRunner::new();
    runner.respond("pi-a", ok_response("done"));
    runner.respond("pi-b", ok_response("done"));
    runner.respond("pb-a", ok_response("done"));
    let (events, sink) = recorded_events();
    let engine = engine_with_progress(
        &runner,
        vec![
            marker_workflow("pi-a", WorkflowPhase::PostImage, WorkflowErrorPolicy::Warn),
            marker_workflow("pi-b", WorkflowPhase::PostImage, WorkflowErrorPolicy::Warn),
            marker_workflow("pb-a", WorkflowPhase::PostBatch, WorkflowErrorPolicy::Warn),
        ],
        2,
        &dir,
        Some(sink),
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_1000.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    engine
        .run_post_image(
            &cancel,
            "raw/DSC_1000.NEF",
            &exported,
            &mut item_artifacts,
            1,
            &dir,
        )
        .expect("postImage succeeds");
    engine
        .run_post_batch(&cancel, &sample_items(&dir).0, &sample_items(&dir).1, &dir)
        .expect("postBatch succeeds");

    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        event_phases(&events),
        vec![
            WorkflowProgressPhase::RunningPostImage,
            WorkflowProgressPhase::RunningPostImage,
            WorkflowProgressPhase::RunningPostBatch,
        ],
        "one event per invocation, in execution order: {recorded:?}"
    );

    let first = &recorded[0];
    assert_eq!(first.run_id, engine.run_id());
    assert_eq!(first.workflow_id.as_deref(), Some("pi-a"));
    assert_eq!(first.source_path.as_deref(), Some("raw/DSC_1000.NEF"));
    assert_eq!(first.index, 1);
    assert_eq!(first.total, 2);
    assert_eq!(first.timeout_seconds, Some(60));
    assert_eq!(first.warning_count, 0);

    let post_batch = &recorded[2];
    assert_eq!(post_batch.run_id, engine.run_id());
    assert_eq!(post_batch.workflow_id.as_deref(), Some("pb-a"));
    assert_eq!(post_batch.source_path, None);
    assert_eq!(post_batch.total, 2);
    assert_eq!(post_batch.timeout_seconds, Some(60));
}

#[test]
fn export_workflow_progress_events_accumulate_warning_counts() {
    let dir = temp_dir("progress-warnings");
    let runner = RecordingRunner::new();
    let mut chatty = ok_response("done");
    chatty.warnings = vec!["w1".to_string(), "w2".to_string()];
    runner.respond("chatty", chatty);
    let mut degraded = ok_response("unreached");
    degraded.warnings = vec!["w3".to_string()];
    runner.respond("degraded", degraded);
    let (events, sink) = recorded_events();
    let engine = engine_with_progress(
        &runner,
        vec![
            marker_workflow(
                "chatty",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
            marker_workflow(
                "degraded",
                WorkflowPhase::PostImage,
                WorkflowErrorPolicy::Warn,
            ),
        ],
        1,
        &dir,
        Some(sink),
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_1001.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    engine
        .run_post_image(
            &cancel,
            "raw/DSC_1001.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("postImage succeeds");

    let recorded = events.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0].warning_count, 0, "nothing warned yet");
    assert_eq!(
        recorded[1].warning_count, 2,
        "the two warnings of the first invocation are visible to the next event"
    );
}

#[test]
fn export_workflow_progress_events_report_waiting_while_the_gate_is_held() {
    let dir = temp_dir("progress-waiting");
    let runner = RecordingRunner::new();
    runner.respond("gated", ok_response("done"));
    let gate = WorkflowConcurrencyGate::new(1);
    let plan = plan_export_workflows(
        &selection(&["gated"]),
        &discovery_from(vec![marker_workflow(
            "gated",
            WorkflowPhase::PostImage,
            WorkflowErrorPolicy::Warn,
        )]),
    )
    .expect("resolves")
    .expect("some");
    let (events, sink) = recorded_events();
    let engine = ExportWorkflowEngine::new(
        plan,
        runner.clone(),
        Arc::new(FakeProber),
        gate.clone(),
        export_settings(),
        dir.clone(),
        vec![script_root()],
        1,
        Some(sink),
    );

    // Hold the only permit so the invocation must wait, then cancel while
    // waiting so the engine settles without ever spawning the workflow.
    let permit = gate.try_acquire().expect("first permit acquired");
    let cancel = Arc::new(AtomicBool::new(false));
    let canceller = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::SeqCst);
        })
    };
    let (exported, artifacts) = write_export_output(&dir, "DSC_1002.jpg");
    let mut item_artifacts = artifacts;
    let error = engine
        .run_post_image(
            &cancel,
            "raw/DSC_1002.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .unwrap_err();
    assert_eq!(
        error,
        rapidraw_lib::export_workflows::WorkflowPhaseError::Cancelled
    );
    assert!(runner.calls.lock().unwrap().is_empty());

    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        event_phases(&events),
        vec![
            WorkflowProgressPhase::RunningPostImage,
            WorkflowProgressPhase::Waiting,
        ],
        "the invocation is announced, then refined while waiting for a slot: {recorded:?}"
    );
    let waiting = &recorded[1];
    assert_eq!(waiting.run_id, engine.run_id());
    assert_eq!(waiting.workflow_id.as_deref(), Some("gated"));
    assert_eq!(waiting.source_path.as_deref(), Some("raw/DSC_1002.NEF"));

    canceller.join().unwrap();
    drop(permit);
}

#[test]
fn export_workflow_run_results_carry_bounded_redacted_stderr_excerpts() {
    let dir = temp_dir("progress-stderr");
    let runner = RecordingRunner::new();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    assert!(!home.is_empty(), "tests require a home directory");
    let noisy_stderr = format!(
        "processing {home}/pictures/DSC_1003.NEF failed\n{}",
        "log-line ".repeat(200)
    );
    runner.respond_with_stderr("noisy", ok_response("done"), noisy_stderr);
    let (events, sink) = recorded_events();
    let engine = engine_with_progress(
        &runner,
        vec![marker_workflow(
            "noisy",
            WorkflowPhase::PostImage,
            WorkflowErrorPolicy::Warn,
        )],
        1,
        &dir,
        Some(sink),
    );

    let (exported, artifacts) = write_export_output(&dir, "DSC_1003.jpg");
    let mut item_artifacts = artifacts;
    let cancel = AtomicBool::new(false);
    let results = engine
        .run_post_image(
            &cancel,
            "raw/DSC_1003.NEF",
            &exported,
            &mut item_artifacts,
            0,
            &dir,
        )
        .expect("postImage succeeds");

    let excerpt = results[0]
        .stderr_excerpt
        .as_deref()
        .expect("stderr excerpt is preserved on the run result");
    assert!(
        excerpt.contains("~/pictures"),
        "home paths are redacted: {excerpt}"
    );
    assert!(!excerpt.contains(&home), "no literal home path: {excerpt}");
    assert!(
        excerpt.chars().count() <= 460,
        "excerpt stays bounded: {} chars",
        excerpt.chars().count()
    );
    // The event itself stays a compact counter, never raw log text.
    let recorded = events.lock().unwrap();
    assert_eq!(recorded[0].warning_count, 0);
}

// ---------------------------------------------------------------------------
// terminal result summary
// ---------------------------------------------------------------------------

#[test]
fn export_result_summary_preserves_item_and_workflow_outcomes() {
    let items = vec![
        ExportItemOutcome {
            source_path: "raw/ok-a.nef".to_string(),
            exported_path: Some("out/ok-a.jpg".to_string()),
            error: None,
        },
        ExportItemOutcome {
            source_path: "raw/ok-b.nef".to_string(),
            exported_path: Some("out/ok-b.jpg".to_string()),
            error: None,
        },
        ExportItemOutcome {
            source_path: "raw/bad.nef".to_string(),
            exported_path: None,
            error: Some("render failed".to_string()),
        },
    ];
    let workflow_runs = vec![
        WorkflowRunResult {
            workflow_id: "warned-wf".to_string(),
            source_path: Some("raw/ok-a.nef".to_string()),
            status: WorkflowRunStatus::Warned,
            message: Some("done with warnings".to_string()),
            warnings: vec!["stale sidecar".to_string()],
            produced_artifacts: Vec::new(),
            stderr_excerpt: None,
        },
        WorkflowRunResult {
            workflow_id: "degraded-wf".to_string(),
            source_path: Some("raw/ok-b.nef".to_string()),
            status: WorkflowRunStatus::Failed,
            message: Some("archive offline".to_string()),
            warnings: Vec::new(),
            produced_artifacts: Vec::new(),
            stderr_excerpt: None,
        },
    ];

    let detail = summarize_export_results("run-1", false, items, workflow_runs);

    assert_eq!(detail.run_id, "run-1");
    assert!(!detail.cancelled);
    assert_eq!(detail.total, 3);
    assert_eq!(detail.succeeded, 2);
    assert_eq!(detail.failed, 1);
    assert_eq!(detail.warned_workflow_runs, 1);
    assert_eq!(detail.failed_workflow_runs, 1);
    assert_eq!(detail.items.len(), 3);
    assert_eq!(detail.items[2].error.as_deref(), Some("render failed"));
    assert_eq!(detail.workflow_runs.len(), 2);
    assert_eq!(detail.workflow_runs[0].status, WorkflowRunStatus::Warned);

    // The payload is typed camelCase for the frontend result view.
    let value = serde_json::to_value(&detail).expect("detail serializes");
    assert_eq!(value["runId"], "run-1");
    assert_eq!(value["warnedWorkflowRuns"], 1);
    assert_eq!(value["failedWorkflowRuns"], 1);
    assert_eq!(value["items"][2]["sourcePath"], "raw/bad.nef");
    assert_eq!(value["workflowRuns"][0]["workflowId"], "warned-wf");
}

#[test]
fn export_result_summary_marks_cancelled_runs_without_inventing_failures() {
    let items = vec![
        ExportItemOutcome {
            source_path: "raw/done.nef".to_string(),
            exported_path: Some("out/done.jpg".to_string()),
            error: None,
        },
        ExportItemOutcome {
            source_path: "raw/skipped.nef".to_string(),
            exported_path: None,
            error: Some("Export cancelled".to_string()),
        },
    ];
    let detail = summarize_export_results("run-2", true, items, Vec::new());
    assert!(detail.cancelled);
    assert_eq!(detail.total, 2);
    assert_eq!(detail.succeeded, 1);
    assert_eq!(detail.failed, 1);
    assert_eq!(detail.warned_workflow_runs, 0);
    assert_eq!(detail.failed_workflow_runs, 0);
}
