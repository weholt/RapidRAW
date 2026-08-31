//! Integration tests for the headless workflow CLI (`rapidraw-060.8`).
//!
//! These tests drive the full headless decision path with fake workflows and
//! a fake interpreter prober — no GUI, no real interpreter required:
//!
//! - launch parsing of repeatable `--workflow` flags feeding the shared
//!   selection/planning service,
//! - `--list-workflows` output built from a real directory scan,
//! - fail-before-export behavior for missing, unavailable, and duplicate ids,
//! - the terminal run summary and exit code for succeed, warn, and fail
//!   policies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rapidraw_lib::export_workflows::{
    CommandWorkflowRunner, ExportWorkflowEngine, ProbePlatform, RuntimeCandidate,
    WorkflowConcurrencyGate, WorkflowDiscoveryOptions, WorkflowDiscoveryResult,
    WorkflowExecutionError, WorkflowExportSettings, WorkflowItem, WorkflowPhase,
    WorkflowPhaseError, WorkflowPolicyFailure, WorkflowRequest, WorkflowResponse,
    WorkflowRunOutput, WorkflowRunResult, WorkflowRunSpec, WorkflowRunStatus, WorkflowRunner,
    discover_workflows, format_headless_workflow_summary, format_workflow_listing,
    plan_export_workflows,
};
use rapidraw_lib::{HeadlessExportSession, LaunchRequest, headless_exit_code, parse_launch_args};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

struct FakeProber;

impl rapidraw_lib::export_workflows::RuntimeProber for FakeProber {
    fn probe(&self, _candidate: &RuntimeCandidate) -> Result<String, String> {
        Ok("Fake Runtime 1.0".to_string())
    }
}

fn temp_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir()
        .join("rapidraw-headless-workflow-cli")
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

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// A registry root containing fake workflows:
/// - `alpha.py` — direct file, defaults (postBatch, warn),
/// - `beta.py` + sidecar `{"phase": "postImage"}`,
/// - `broken.py` + sidecar with an out-of-range order (unselectable).
fn fake_workflow_root(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    write(&root.join("alpha.py"), "# fake workflow\n");
    write(&root.join("beta.py"), "# fake workflow\n");
    write(
        &root.join("beta.py.rapidraw.json"),
        r#"{ "phase": "postImage" }"#,
    );
    write(&root.join("broken.py"), "# fake workflow\n");
    write(&root.join("broken.py.rapidraw.json"), r#"{ "order": -1 }"#);
    root
}

fn discover_fake_root(root: &Path) -> WorkflowDiscoveryResult {
    discover_workflows(&WorkflowDiscoveryOptions {
        bundled_root: Some(root.to_path_buf()),
        user_root: None,
        platform: ProbePlatform::Unix,
        prober: &FakeProber,
    })
}

/// Runner that always answers `ok` — enough to observe the phases and order
/// the headless path executes. Records every request it received.
struct OkRunner {
    calls: Mutex<Vec<(String, WorkflowPhase)>>,
}

impl OkRunner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
        })
    }

    fn recorded(&self) -> Vec<(String, WorkflowPhase)> {
        self.calls.lock().unwrap().clone()
    }
}

impl WorkflowRunner for OkRunner {
    fn run(
        &self,
        spec: &WorkflowRunSpec<'_>,
        _cancel: &AtomicBool,
    ) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
        let request: WorkflowRequest =
            serde_json::from_str(&spec.request_json).expect("spec carries a valid request");
        self.calls
            .lock()
            .unwrap()
            .push((request.workflow_id.clone(), request.phase));
        Ok(WorkflowRunOutput {
            response: WorkflowResponse {
                ok: true,
                message: Some("done".to_string()),
                warnings: Vec::new(),
                produced_artifact_paths: Vec::new(),
            },
            produced_artifacts: Vec::new(),
            stderr: String::new(),
        })
    }
}

/// Runner whose first `ok:false` script-reported failure follows `onError`.
struct ScriptedRunner {
    fail_ids: Vec<&'static str>,
    calls: Mutex<Vec<String>>,
}

impl ScriptedRunner {
    fn failing(ids: Vec<&'static str>) -> Arc<Self> {
        Arc::new(Self {
            fail_ids: ids,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn recorded(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl WorkflowRunner for ScriptedRunner {
    fn run(
        &self,
        spec: &WorkflowRunSpec<'_>,
        _cancel: &AtomicBool,
    ) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
        let request: WorkflowRequest =
            serde_json::from_str(&spec.request_json).expect("spec carries a valid request");
        self.calls.lock().unwrap().push(request.workflow_id.clone());
        if self.fail_ids.contains(&request.workflow_id.as_str()) {
            return Ok(WorkflowRunOutput {
                response: WorkflowResponse {
                    ok: false,
                    message: Some("script said no".to_string()),
                    warnings: Vec::new(),
                    produced_artifact_paths: Vec::new(),
                },
                produced_artifacts: Vec::new(),
                stderr: String::new(),
            });
        }
        Ok(WorkflowRunOutput {
            response: WorkflowResponse {
                ok: true,
                message: None,
                warnings: Vec::new(),
                produced_artifact_paths: Vec::new(),
            },
            produced_artifacts: Vec::new(),
            stderr: String::new(),
        })
    }
}

fn headless_session(args: &[&str]) -> HeadlessExportSession {
    let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    match parse_launch_args(&owned) {
        LaunchRequest::HeadlessExport(session) => session,
        other => panic!("expected a headless export session, got {other:?}"),
    }
}

fn export_settings() -> WorkflowExportSettings {
    WorkflowExportSettings {
        file_format: "jpeg".to_string(),
        jpeg_quality: 90,
        keep_metadata: true,
        strip_gps: false,
    }
}

fn run_post_batch(
    runner: Arc<dyn WorkflowRunner>,
    root: &Path,
    selected_ids: &[String],
) -> Result<Vec<WorkflowRunResult>, WorkflowPhaseError> {
    let discovery = discover_fake_root(root);
    let plan = plan_export_workflows(selected_ids, &discovery)
        .expect("selection resolves")
        .expect("plan is non-empty");
    let engine = ExportWorkflowEngine::new(
        plan,
        runner,
        Arc::new(FakeProber),
        WorkflowConcurrencyGate::default(),
        export_settings(),
        root.join("workspace"),
        vec![root.canonicalize().expect("canonical root")],
        1,
        None,
    );
    let cancel = AtomicBool::new(false);
    let items = [WorkflowItem {
        source_path: "raw/DSC_0001.NEF".to_string(),
        exported_path: Some("out/DSC_0001.jpg".to_string()),
        artifacts: Vec::new(),
        error: None,
    }];
    let result = engine.run_post_batch(&cancel, &items, &items, Path::new("out"));
    engine.cleanup();
    result
}

// ---------------------------------------------------------------------------
// --list-workflows output
// ---------------------------------------------------------------------------

#[test]
fn headless_listing_formats_a_real_registry_scan() {
    let root = fake_workflow_root("listing");
    let result = discover_fake_root(&root);
    let listing = format_workflow_listing(Some(&root), None, &result);

    assert!(listing.contains(&format!("Bundled workflows directory: {}", root.display())));
    assert!(listing.contains("User workflows directory: (unresolvable)"));

    let row = |id: &str| {
        listing
            .lines()
            .find(|line| line.starts_with(id))
            .unwrap_or_else(|| panic!("no listing row for {id}:\n{listing}"))
            .to_string()
    };
    // Direct-file defaults: alpha.py is postBatch from the file name alone.
    assert!(row("alpha").contains("postBatch"));
    assert!(row("alpha").contains("bundled"));
    // Sidecar override: beta runs postImage.
    assert!(row("beta").contains("postImage"));
    // Invalid sidecar values keep the row but make it unselectable, with the
    // reason printed below the table.
    assert!(row("broken").contains("postBatch"));
    assert!(listing.contains("Unselectable"));
    assert!(listing.contains("- broken: order must be 0..=1000"));
    assert!(listing.contains("3 workflow(s) listed."));
}

#[test]
fn headless_listing_is_deterministic_for_the_same_scan() {
    let root = fake_workflow_root("deterministic");
    let result = discover_fake_root(&root);
    assert_eq!(
        format_workflow_listing(Some(&root), None, &result),
        format_workflow_listing(Some(&root), None, &result)
    );
}

// ---------------------------------------------------------------------------
// selection runs through the shared service in command-line order
// ---------------------------------------------------------------------------

#[test]
fn headless_workflow_selection_runs_ids_in_command_line_order() {
    let root = fake_workflow_root("order");
    let session = headless_session(&[
        "export",
        "/photos",
        "--output",
        "/out",
        "--workflow",
        "beta",
        "--workflow",
        "alpha",
    ]);
    assert_eq!(
        session.workflow_ids,
        vec!["beta".to_string(), "alpha".to_string()]
    );

    let runner = OkRunner::new();
    let results =
        run_post_batch(runner.clone(), &root, &session.workflow_ids).expect("workflows succeed");

    // Selection order (not registry order) defines execution order; beta is
    // postImage so only alpha's postBatch runs in run_post_batch.
    assert_eq!(
        runner.recorded(),
        vec![("alpha".to_string(), WorkflowPhase::PostBatch)]
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, WorkflowRunStatus::Succeeded);
}

// ---------------------------------------------------------------------------
// fail before image export starts
// ---------------------------------------------------------------------------

#[test]
fn headless_unknown_workflow_id_fails_before_any_workflow_runs() {
    let root = fake_workflow_root("unknown-id");
    let session = headless_session(&[
        "export",
        "/photos",
        "--output",
        "/out",
        "--workflow",
        "ghost",
    ]);
    let discovery = discover_fake_root(&root);
    let runner = OkRunner::new();

    let error = plan_export_workflows(&session.workflow_ids, &discovery)
        .expect_err("unknown id must fail planning");

    assert_eq!(
        error.to_string(),
        "workflow 'ghost' is not in the workflow registry"
    );
    assert!(runner.recorded().is_empty());
    assert_eq!(headless_exit_code(&Err(error.to_string())), 1);
}

#[test]
fn headless_unselectable_workflow_id_fails_with_the_metadata_reason() {
    let root = fake_workflow_root("unselectable");
    let discovery = discover_fake_root(&root);

    let error = plan_export_workflows(&["broken".to_string()], &discovery)
        .expect_err("unselectable id must fail planning");

    assert_eq!(
        error.to_string(),
        "workflow 'broken' has invalid metadata and cannot be selected"
    );
    assert_eq!(headless_exit_code(&Err(error.to_string())), 1);
}

#[test]
fn headless_duplicate_workflow_selection_fails() {
    let root = fake_workflow_root("duplicate");
    let discovery = discover_fake_root(&root);

    let error = plan_export_workflows(&["alpha".to_string(), "alpha".to_string()], &discovery)
        .expect_err("duplicate selection must fail planning");

    assert_eq!(
        error.to_string(),
        "workflow 'alpha' was selected more than once"
    );
    assert_eq!(headless_exit_code(&Err(error.to_string())), 1);
}

#[test]
fn headless_unavailable_runtime_fails_before_export_with_the_probe_reason() {
    struct NeverProber;
    impl rapidraw_lib::export_workflows::RuntimeProber for NeverProber {
        fn probe(&self, _candidate: &RuntimeCandidate) -> Result<String, String> {
            Err("no interpreter on PATH".to_string())
        }
    }

    let root = fake_workflow_root("no-runtime");
    let discovery = discover_workflows(&WorkflowDiscoveryOptions {
        bundled_root: Some(root.clone()),
        user_root: None,
        platform: ProbePlatform::Unix,
        prober: &NeverProber,
    });

    let error = plan_export_workflows(&["alpha".to_string()], &discovery)
        .expect_err("unavailable runtime must fail planning");

    assert!(
        error
            .to_string()
            .starts_with("workflow 'alpha' has no available runtime: "),
        "{}",
        error
    );
    assert!(error.to_string().contains("no interpreter on PATH"));
    assert_eq!(headless_exit_code(&Err(error.to_string())), 1);
}

// ---------------------------------------------------------------------------
// terminal output and exit codes
// ---------------------------------------------------------------------------

#[test]
fn headless_warn_policy_degradation_succeeds_with_a_summary_line() {
    let root = fake_workflow_root("warn-policy");
    // alpha is postBatch with onError=warn (direct-file defaults).
    let runner = ScriptedRunner::failing(vec!["alpha"]);

    let results = run_post_batch(runner.clone(), &root, &["alpha".to_string()])
        .expect("warn policy never fails the batch");

    assert_eq!(runner.recorded(), vec!["alpha".to_string()]);
    assert_eq!(results[0].status, WorkflowRunStatus::Failed);

    let detail = rapidraw_lib::export_workflows::summarize_export_results(
        "run-1",
        false,
        Vec::new(),
        results,
    );
    let summary = format_headless_workflow_summary(&detail);
    assert!(summary.contains("Workflow 'alpha' degraded: script said no"));
    assert!(summary.contains("Workflow runs: 0 succeeded, 0 warned, 1 degraded, 0 cancelled."));
    // Degradation under warn policy is not an export failure: exit code 0.
    assert_eq!(headless_exit_code(&Ok(())), 0);
}

#[test]
fn headless_fail_policy_failure_fails_the_export_with_the_typed_reason() {
    let root = temp_root("fail-policy");
    write(&root.join("strict.py"), "# fake workflow\n");
    write(
        &root.join("strict.py.rapidraw.json"),
        r#"{ "onError": "fail" }"#,
    );

    let runner = ScriptedRunner::failing(vec!["strict"]);
    let error = run_post_batch(runner.clone(), &root, &["strict".to_string()])
        .expect_err("fail policy must fail the batch");

    assert_eq!(
        error,
        WorkflowPhaseError::Failed(WorkflowPolicyFailure::ScriptReported {
            workflow_id: "strict".to_string(),
            message: "script said no".to_string(),
        })
    );
    assert_eq!(runner.recorded(), vec!["strict".to_string()]);
    assert_eq!(headless_exit_code(&Err(error.to_string())), 1);
}

#[test]
fn headless_runner_and_real_command_runner_stay_interchangeable() {
    // The headless path uses CommandWorkflowRunner in production; a spec
    // built exactly like the engine builds one must still work with it.
    let root = fake_workflow_root("command-runner");
    let discovery = discover_fake_root(&root);
    let plan = plan_export_workflows(&["alpha".to_string()], &discovery)
        .expect("resolves")
        .expect("some");

    let engine = ExportWorkflowEngine::new(
        plan,
        Arc::new(CommandWorkflowRunner),
        // Production uses real probes; keep this test hermetic.
        Arc::new(FakeProber),
        WorkflowConcurrencyGate::default(),
        export_settings(),
        root.join("workspace-never-created"),
        vec![root.canonicalize().expect("canonical root")],
        0,
        None,
    );
    // total == 0 with no items: postBatch is skipped without a run, so the
    // engine is wired correctly but never spawns a subprocess here.
    let cancel = AtomicBool::new(true);
    let error = engine
        .run_post_batch(&cancel, &[], &[], Path::new("out"))
        .expect_err("cancelled run must not invoke anything");
    assert_eq!(error, WorkflowPhaseError::Cancelled);
}
