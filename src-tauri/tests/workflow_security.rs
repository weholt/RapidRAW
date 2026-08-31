//! Security-focused integration tests for workflow discovery and execution
//! (`rapidraw-060.9`).
//!
//! Every test is named `workflow_security_*` so
//! `cargo test --manifest-path src-tauri/Cargo.toml workflow_security`
//! runs exactly this pass on Windows and Unix CI. Coverage:
//!
//! - canonical path containment and sibling-prefix escapes (real filesystem),
//! - symlink/reparse-point escapes at discovery **and** spawn time,
//! - interpreter resolution pinned through `PATH` between probe and spawn,
//! - environment leakage from the parent into children and results,
//! - argument-array path quoting for hostile file names,
//! - process-tree cleanup on output exhaustion,
//! - duplicate ids and malformed/hostile metadata failing closed,
//! - packaged bundled examples staying inside their root.
//!
//! Boundaries these tests deliberately do not claim: workflows are trusted
//! local code; in-place content replacement of a still-valid path, and an
//! attacker who can already write to `PATH` or the workflow roots, are
//! outside the threat model (see `docs/decisions/export-workflow-protocol.md`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rapidraw_lib::export_workflows::{
    CommandWorkflowRunner, ExportWorkflowEngine, ProbePlatform, RuntimeCandidate, RuntimeProber,
    WorkflowConcurrencyGate, WorkflowDiscoveryOptions, WorkflowErrorPolicy, WorkflowExecutionError,
    WorkflowExportSettings, WorkflowItem, WorkflowPhase, WorkflowPhaseError, WorkflowPolicyFailure,
    WorkflowResponse, WorkflowRunOutput, WorkflowRunSpec, WorkflowRunStatus, WorkflowRunner,
    discover_workflows, plan_export_workflows, resolve_program_on_path,
    validate_workflow_script_identity,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn temp_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir()
        .join("rapidraw-workflow-security")
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

fn probe_platform() -> ProbePlatform {
    if cfg!(windows) {
        ProbePlatform::Windows
    } else {
        ProbePlatform::Unix
    }
}

/// Prober that reports every candidate available; discovery and interpreter
/// resolution stay hermetic — no real interpreter is needed unless a test
/// spawns one explicitly.
struct AlwaysProber;

impl RuntimeProber for AlwaysProber {
    fn probe(&self, _candidate: &RuntimeCandidate) -> Result<String, String> {
        Ok("Fake Runtime 1.0".to_string())
    }
}

fn discover_root(root: &Path) -> rapidraw_lib::export_workflows::WorkflowDiscoveryResult {
    discover_workflows(&WorkflowDiscoveryOptions {
        bundled_root: Some(root.to_path_buf()),
        user_root: None,
        platform: probe_platform(),
        prober: &AlwaysProber,
    })
}

/// Canonical form without the Windows verbatim (`\\?\`) prefix, matching how
/// discovery reports script paths.
fn canonical_stripped(path: &Path) -> PathBuf {
    let text = path
        .canonicalize()
        .expect("canonicalize")
        .to_string_lossy()
        .to_string();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string())
}

fn diagnostic_codes(result: &rapidraw_lib::export_workflows::WorkflowDiscoveryResult) -> Vec<&str> {
    result.diagnostics.iter().map(|d| d.code.as_str()).collect()
}

/// Runner that records the script path of every invocation and always
/// succeeds; used to observe whether spawn-time revalidation let a request
/// through.
struct RecordingRunner {
    calls: Mutex<Vec<String>>,
}

impl RecordingRunner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
        })
    }

    fn script_paths(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl WorkflowRunner for RecordingRunner {
    fn run(
        &self,
        spec: &WorkflowRunSpec<'_>,
        _cancel: &AtomicBool,
    ) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
        self.calls
            .lock()
            .unwrap()
            .push(spec.script_path.to_string_lossy().to_string());
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

fn export_settings() -> WorkflowExportSettings {
    WorkflowExportSettings {
        file_format: "jpeg".to_string(),
        jpeg_quality: 90,
        keep_metadata: true,
        strip_gps: false,
    }
}

/// Builds an engine over a real discovered root with the given identity
/// roots, selecting every discovered workflow with the given error policy
/// so identity failures surface exactly as each test needs.
fn engine_over_root(
    root: &Path,
    runner: Arc<dyn WorkflowRunner>,
    identity_roots: Vec<PathBuf>,
    workspace_parent: &Path,
    on_error: WorkflowErrorPolicy,
) -> ExportWorkflowEngine {
    let mut discovery = discover_root(root);
    assert!(
        !discovery.workflows.is_empty(),
        "test root must discover at least one workflow"
    );
    for workflow in &mut discovery.workflows {
        workflow.on_error = on_error;
    }
    let ids: Vec<String> = discovery
        .workflows
        .iter()
        .map(|workflow| workflow.id.clone())
        .collect();
    let plan = plan_export_workflows(&ids, &discovery)
        .expect("selection resolves")
        .expect("plan is non-empty");
    ExportWorkflowEngine::new(
        plan,
        runner,
        Arc::new(AlwaysProber),
        WorkflowConcurrencyGate::default(),
        export_settings(),
        workspace_parent.to_path_buf(),
        identity_roots,
        1,
        None,
    )
}

fn run_first_post_batch(engine: &ExportWorkflowEngine, workspace: &Path) -> Result<(), String> {
    let cancel = AtomicBool::new(false);
    let items = [WorkflowItem {
        source_path: "raw/DSC_0001.NEF".to_string(),
        exported_path: Some("out/DSC_0001.jpg".to_string()),
        artifacts: Vec::new(),
        error: None,
    }];
    engine
        .run_post_batch(&cancel, &items, &items, workspace)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// One platform-specific fake executable launched directly (no shell):
/// an executable `#!/bin/sh` script on Unix, a `.ps1` through
/// `powershell -File` on Windows. Neither involves a command string.
fn fake_executable(dir: &Path, stem: &str, _sh_body: &str, _ps1_body: &str) -> PathBuf {
    #[cfg(unix)]
    let path = dir.join(format!("{stem}.py"));
    #[cfg(windows)]
    let path = dir.join(format!("{stem}.ps1"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fake dir");
    }
    #[cfg(unix)]
    {
        std::fs::write(&path, format!("#!/bin/sh\n{_sh_body}\n"))
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    }
    #[cfg(windows)]
    {
        std::fs::write(&path, _ps1_body)
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    path
}

#[cfg(unix)]
fn fake_interpreter_for(script: &Path) -> rapidraw_lib::export_workflows::WorkflowInterpreter {
    rapidraw_lib::export_workflows::WorkflowInterpreter {
        language: rapidraw_lib::export_workflows::WorkflowLanguage::Python,
        program: script.to_string_lossy().to_string(),
        prefix_args: Vec::new(),
    }
}

#[cfg(windows)]
fn fake_interpreter_for(_script: &Path) -> rapidraw_lib::export_workflows::WorkflowInterpreter {
    rapidraw_lib::export_workflows::WorkflowInterpreter {
        language: rapidraw_lib::export_workflows::WorkflowLanguage::Python,
        program: "powershell".to_string(),
        prefix_args: vec![
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
        ],
    }
}

fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    path.exists()
}

fn wait_for_process_exit(pid: u32) -> bool {
    let mut system = sysinfo::System::new();
    for _ in 0..150 {
        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        if system.process(sysinfo::Pid::from_u32(pid)).is_none() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn make_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let created = std::os::windows::fs::symlink_file(target, link);
    match created {
        Ok(()) => true,
        Err(error) => {
            // Symlink creation requires privileges on some Windows setups;
            // the rejection path is exercised wherever creation succeeds.
            eprintln!("skipping symlink case, creation unavailable: {error}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// discovery: containment, symlinks, hostile names
// ---------------------------------------------------------------------------

/// Sibling directories with a shared prefix (`workflows` vs `workflows-2`)
/// must never satisfy containment; canonical script paths inside the real
/// root must. Filesystem-level companion to the component-wise unit test.
#[test]
fn workflow_security_discovery_containment_holds_on_the_real_filesystem() {
    let base = temp_root("containment");
    let root = base.join("workflows");
    std::fs::create_dir_all(&root).expect("create root");
    std::fs::create_dir_all(base.join("workflows-2")).expect("create sibling");
    write(&root.join("inside.py"), "# ok\n");
    write(&base.join("workflows-2").join("outsider.py"), "# evil\n");

    let result = discover_root(&base.join("workflows"));
    let canonical_root = canonical_stripped(&root);

    let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["inside"],
        "only the entry inside the root is listed"
    );
    let script = PathBuf::from(&result.workflows[0].script_path);
    assert_eq!(
        script,
        canonical_root.join("inside.py"),
        "reported script paths are canonical"
    );
    assert!(script.starts_with(&canonical_root));
    assert!(
        !script.starts_with(base.join("workflows-2")),
        "a sibling-prefix directory must not satisfy containment"
    );
}

/// Symlinked workflow scripts and metadata sidecars are rejected at
/// discovery, on Windows (reparse points) and Unix, without executing
/// anything. `rapidraw-060.9`.
#[test]
fn workflow_security_discovery_rejects_symlink_scripts_and_sidecars() {
    let base = temp_root("discovery-symlink");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).expect("create outside");
    write(&outside.join("evil.py"), "print('pwned')\n");
    write(&outside.join("evil.json"), r#"{ "id": "evil-sidecar" }"#);

    let linked_script = root.join("linked.py");
    if !make_symlink(&outside.join("evil.py"), &linked_script) {
        return;
    }
    let linked_sidecar = root.join("linked.py.rapidraw.json");
    if !make_symlink(&outside.join("evil.json"), &linked_sidecar) {
        return;
    }

    let result = discover_root(&root);
    assert!(
        result.workflows.is_empty(),
        "a symlinked script must never be listed: {:?}",
        result.workflows
    );
    let codes = diagnostic_codes(&result);
    assert!(
        codes.contains(&"workflow.discovery.symlink.rejected"),
        "{codes:?}"
    );
}

/// Hostile but legal file names (spaces, ampersands, dollar signs, Unicode,
/// mixed case, doubled separators-normalizing stems) must be discovered with
/// normalized ids, canonical in-root script paths, and remain selectable.
#[test]
fn workflow_security_discovery_handles_hostile_file_names() {
    let base = temp_root("hostile-names");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    // One name valid on both Windows and Unix with shell metacharacters.
    write(
        &root.join("weird & name'é「算」'; $x.py"),
        "# hostile name\n",
    );
    write(&root.join("UPPER Case.PY"), "# mixed case extension\n");
    write(&root.join("a  b -- c.js"), "# collapsed separators\n");

    let result = discover_root(&root);
    let canonical_root = canonical_stripped(&root);
    assert_eq!(result.workflows.len(), 3, "{:?}", result.workflows);

    let by_id = |id: &str| {
        result
            .workflows
            .iter()
            .find(|w| w.id == id)
            .unwrap_or_else(|| panic!("no workflow '{id}': {:?}", result.workflows))
    };
    // Ids are normalized to stable [a-z0-9-] regardless of the raw name;
    // non-ASCII and punctuation act as separators.
    let weird = by_id("weird-name-x");
    assert!(weird.selectable);
    assert_eq!(
        PathBuf::from(&weird.script_path),
        canonical_root.join("weird & name'é「算」'; $x.py")
    );
    assert!(by_id("upper-case").selectable);
    assert!(by_id("a-b-c").selectable);
    for workflow in &result.workflows {
        let script = PathBuf::from(&workflow.script_path);
        assert!(
            script.starts_with(&canonical_root),
            "{} must stay inside the root",
            script.display()
        );
    }
}

/// Duplicate ids derived from distinct hostile names shadow deterministically
/// (lexicographically smallest canonical path wins) and are reported.
#[test]
fn workflow_security_discovery_duplicate_ids_from_distinct_names_fail_closed() {
    let root = temp_root("duplicate-ids");
    write(&root.join("A B.py"), "# normalizes to a-b\n");
    write(&root.join("a-b.py"), "# also a-b\n");

    let result = discover_root(&root);
    let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids, vec!["a-b"], "exactly one survivor per id");
    let codes = diagnostic_codes(&result);
    assert!(
        codes.contains(&"workflow.discovery.duplicate.id"),
        "{codes:?}"
    );
}

/// Hostile metadata (invalid id, out-of-range order and timeout, oversize
/// display name, malformed JSON, wrong field types) marks entries
/// unselectable with typed diagnostics instead of poisoning the registry.
#[test]
fn workflow_security_discovery_hostile_metadata_fails_closed() {
    let root = temp_root("hostile-metadata");
    write(&root.join("bad-id.py"), "# x\n");
    write(
        &root.join("bad-id.py.rapidraw.json"),
        r#"{ "id": "Bad Id!", "order": 5000, "timeoutSeconds": 0 }"#,
    );
    write(&root.join("bad-json.py"), "# x\n");
    write(&root.join("bad-json.py.rapidraw.json"), "{ not json");
    write(&root.join("wrong-type.py"), "# x\n");
    write(
        &root.join("wrong-type.py.rapidraw.json"),
        r#"{ "order": "high" }"#,
    );
    let long_name = "x".repeat(500);
    write(&root.join("long-name.py"), "# x\n");
    write(
        &root.join("long-name.py.rapidraw.json"),
        &format!(r#"{{ "displayName": "{long_name}" }}"#),
    );
    write(&root.join("healthy.py"), "# x\n");

    let result = discover_root(&root);
    let by_id = |id: &str| {
        result
            .workflows
            .iter()
            .find(|w| w.id == id)
            .unwrap_or_else(|| panic!("no workflow '{id}': {:?}", result.workflows))
    };
    for hostile in ["bad-id", "bad-json", "wrong-type", "long-name"] {
        assert!(!by_id(hostile).selectable, "{hostile} must be unselectable");
    }
    assert!(by_id("healthy").selectable);

    let codes = diagnostic_codes(&result);
    assert!(
        codes.contains(&"workflow.discovery.metadata.invalid"),
        "{codes:?}"
    );
    let bad_id = by_id("bad-id");
    let entry_codes: Vec<&str> = bad_id.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(
        entry_codes.contains(&"workflow.metadata.id.invalid"),
        "{entry_codes:?}"
    );
    assert!(
        entry_codes.contains(&"workflow.metadata.order.outOfRange"),
        "{entry_codes:?}"
    );
    assert!(
        entry_codes.contains(&"workflow.metadata.timeoutSeconds.outOfRange"),
        "{entry_codes:?}"
    );
    let long_name_entry = by_id("long-name");
    let long_codes: Vec<&str> = long_name_entry
        .diagnostics
        .iter()
        .map(|d| d.code.as_str())
        .collect();
    assert!(
        long_codes.contains(&"workflow.metadata.displayName.tooLong"),
        "{long_codes:?}"
    );
}

// ---------------------------------------------------------------------------
// spawn-time script identity revalidation
// ---------------------------------------------------------------------------

fn post_batch_identity_error(
    engine: &ExportWorkflowEngine,
    workspace: &Path,
) -> WorkflowPhaseError {
    let cancel = AtomicBool::new(false);
    let items = [WorkflowItem {
        source_path: "raw/DSC_0001.NEF".to_string(),
        exported_path: Some("out/DSC_0001.jpg".to_string()),
        artifacts: Vec::new(),
        error: None,
    }];
    engine
        .run_post_batch(&cancel, &items, &items, workspace)
        .expect_err("identity failure must fail the fail-policy batch")
}

/// An intact script still validates; the invocation reaches the runner with
/// the exact canonical script path as one argv entry (path quoting).
#[test]
fn workflow_security_spawn_intact_script_revalidates_and_reaches_the_runner() {
    let base = temp_root("identity-intact");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    write(&root.join("intact & trusted.py"), "# still here\n");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let runner = RecordingRunner::new();
    let engine = engine_over_root(
        &root,
        runner.clone(),
        vec![canonical_stripped(&root)],
        &workspace,
        WorkflowErrorPolicy::Fail,
    );

    run_first_post_batch(&engine, &workspace).expect("intact script runs");
    let paths = runner.script_paths();
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0].ends_with("intact & trusted.py"),
        "hostile-name script path passed as one literal argv entry: {paths:?}"
    );
}

/// A script replaced by a symlink after discovery (pointing outside the
/// workflow root) must be refused at spawn time and never reach a runner.
#[test]
fn workflow_security_spawn_rejects_script_replaced_by_symlink_after_discovery() {
    let base = temp_root("identity-symlink");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).expect("create outside");
    write(&outside.join("evil.py"), "print('pwned')\n");
    let script = root.join("victim.py");
    write(&script, "# original\n");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let runner = RecordingRunner::new();
    let engine = engine_over_root(
        &root,
        runner.clone(),
        vec![canonical_stripped(&root)],
        &workspace,
        WorkflowErrorPolicy::Fail,
    );

    // Substitute after the engine snapshot, before the invocation.
    std::fs::remove_file(&script).expect("remove original");
    if !make_symlink(&outside.join("evil.py"), &script) {
        return;
    }

    let error = post_batch_identity_error(&engine, &workspace);
    match error {
        WorkflowPhaseError::Failed(WorkflowPolicyFailure::ExecutionError {
            error: WorkflowExecutionError::ScriptIdentity { reason },
            ..
        }) => {
            assert!(reason.contains("symlink"), "{reason}");
        }
        other => panic!("expected ScriptIdentity failure, got {other:?}"),
    }
    assert!(
        runner.script_paths().is_empty(),
        "no subprocess may be launched for a substituted script"
    );
    assert!(
        !outside.join("pwned.marker").exists(),
        "the substituted payload must never execute"
    );
}

/// A script deleted between discovery and spawn fails closed with the typed
/// identity error instead of spawning an interpreter for a missing file.
#[test]
fn workflow_security_spawn_rejects_deleted_script_after_discovery() {
    let base = temp_root("identity-deleted");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    let script = root.join("gone.py");
    write(&script, "# transient\n");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let runner = RecordingRunner::new();
    let engine = engine_over_root(
        &root,
        runner.clone(),
        vec![canonical_stripped(&root)],
        &workspace,
        WorkflowErrorPolicy::Fail,
    );

    std::fs::remove_file(&script).expect("delete script");
    let error = post_batch_identity_error(&engine, &workspace);
    match error {
        WorkflowPhaseError::Failed(WorkflowPolicyFailure::ExecutionError {
            error: WorkflowExecutionError::ScriptIdentity { reason },
            ..
        }) => {
            assert!(reason.contains("no longer readable"), "{reason}");
        }
        other => panic!("expected ScriptIdentity failure, got {other:?}"),
    }
    assert!(runner.script_paths().is_empty());
}

/// A script that is intact but outside the engine's configured identity
/// roots is refused (containment revalidated at spawn, not just discovery).
#[test]
fn workflow_security_spawn_rejects_scripts_outside_the_configured_roots() {
    let base = temp_root("identity-outside");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    write(&root.join("fine.py"), "# fine but not trusted here\n");
    let decoy = base.join("decoy-root");
    std::fs::create_dir_all(&decoy).expect("create decoy root");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let runner = RecordingRunner::new();
    let engine = engine_over_root(
        &root,
        runner.clone(),
        // Deliberately different roots than where the script lives.
        vec![decoy.canonicalize().expect("canonical decoy")],
        &workspace,
        WorkflowErrorPolicy::Fail,
    );

    let error = post_batch_identity_error(&engine, &workspace);
    match error {
        WorkflowPhaseError::Failed(WorkflowPolicyFailure::ExecutionError {
            error: WorkflowExecutionError::ScriptIdentity { reason },
            ..
        }) => {
            assert!(reason.contains("outside every workflow root"), "{reason}");
        }
        other => panic!("expected ScriptIdentity failure, got {other:?}"),
    }
    assert!(runner.script_paths().is_empty());
}

/// Under the warn policy an identity failure degrades the run result (typed
/// status and message) instead of failing the item, and still never spawns.
#[test]
fn workflow_security_spawn_identity_failure_under_warn_policy_degrades() {
    let base = temp_root("identity-warn");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    let script = root.join("flaky.py");
    write(&script, "# here, then gone\n");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let runner = RecordingRunner::new();
    let engine = engine_over_root(
        &root,
        runner.clone(),
        vec![canonical_stripped(&root)],
        &workspace,
        WorkflowErrorPolicy::Warn,
    );

    std::fs::remove_file(&script).expect("delete script");
    let cancel = AtomicBool::new(false);
    let items = [WorkflowItem {
        source_path: "raw/DSC_0001.NEF".to_string(),
        exported_path: Some("out/DSC_0001.jpg".to_string()),
        artifacts: Vec::new(),
        error: None,
    }];
    let results = engine
        .run_post_batch(&cancel, &items, &items, &workspace)
        .expect("warn policy tolerates the identity failure");
    assert_eq!(results[0].status, WorkflowRunStatus::Failed);
    assert!(
        results[0]
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("no longer matches its discovered identity"),
        "{:?}",
        results[0].message
    );
    assert!(runner.script_paths().is_empty());
}

/// `validate_workflow_script_identity` unit pass over verbatim Windows
/// forms and symlink swap attempts at the leaf level.
#[test]
fn workflow_security_identity_validation_unit_cases() {
    let base = temp_root("identity-unit");
    let root = base.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    let script = root.join("plain.py");
    write(&script, "# plain\n");
    let canonical_root = canonical_stripped(&root);
    let canonical_script = canonical_stripped(&script);

    assert_eq!(
        validate_workflow_script_identity(&canonical_script, std::slice::from_ref(&canonical_root))
            .expect("intact script validates"),
        canonical_script
    );

    // Empty roots fail closed.
    let error = validate_workflow_script_identity(&canonical_script, &[]).unwrap_err();
    assert!(matches!(
        error,
        WorkflowExecutionError::ScriptIdentity { .. }
    ));

    // Non-file targets fail.
    let error =
        validate_workflow_script_identity(&canonical_root, std::slice::from_ref(&canonical_root))
            .unwrap_err();
    assert!(matches!(
        error,
        WorkflowExecutionError::ScriptIdentity { .. }
    ));

    // A symlink at the recorded path fails even when the target is valid
    // and inside the same root.
    let linked = root.join("linked.py");
    if make_symlink(&canonical_script, &linked) {
        let error = validate_workflow_script_identity(&linked, &[canonical_root]).unwrap_err();
        match error {
            WorkflowExecutionError::ScriptIdentity { reason } => {
                assert!(reason.contains("symlink"), "{reason}");
            }
            other => panic!("expected ScriptIdentity, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// interpreter resolution between probe and spawn
// ---------------------------------------------------------------------------

/// Bare interpreter names are resolved through `PATH` exactly once at spawn:
/// first directory hit wins, `PATHEXT` extensions apply on Windows, the
/// executable bit applies on Unix, and path-like names are never treated as
/// bare lookups.
#[test]
fn workflow_security_interpreter_path_resolution_first_hit_and_platform_rules() {
    let base = temp_root("interpreter-path");
    let first = base.join("first");
    let second = base.join("second");
    std::fs::create_dir_all(&first).expect("create first");
    std::fs::create_dir_all(&second).expect("create second");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let a = first.join("rapidraw-fake-tool");
        std::fs::write(&a, "#!/bin/sh\n").expect("write a");
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o755)).expect("chmod a");
        // A non-executable file earlier on PATH must be skipped, proving the
        // executable-bit rule rather than a naive first-file-wins search.
        let dead = first.join("rapidraw-fake-dead-tool");
        std::fs::write(&dead, "not executable\n").expect("write dead");
        std::fs::set_permissions(&dead, std::fs::Permissions::from_mode(0o644))
            .expect("unexecutable");

        let path_value = format!("{}:{}", first.display(), second.display());
        assert_eq!(
            resolve_program_on_path("rapidraw-fake-tool", &path_value, None),
            Some(a)
        );
        assert_eq!(
            resolve_program_on_path("rapidraw-fake-dead-tool", &path_value, None),
            None,
            "non-executable files are skipped on Unix"
        );
        assert_eq!(
            resolve_program_on_path("rapidraw-missing-tool", &path_value, None),
            None
        );
    }
    #[cfg(windows)]
    {
        let exe = first.join("rapidraw-fake-tool.EXE");
        std::fs::write(&exe, b"fake").expect("write exe");
        let path_value = format!("{};{}", first.display(), second.display());
        // Default extension order: the .EXE in the first directory wins.
        assert_eq!(
            resolve_program_on_path("rapidraw-fake-tool", &path_value, None),
            Some(exe)
        );
        // A custom PATHEXT changes the extension searched within each
        // directory, here finding the .TXT in the second one.
        let txt = second.join("rapidraw-fake-order.TXT");
        std::fs::write(&txt, b"fake-txt").expect("write txt");
        assert_eq!(
            resolve_program_on_path("rapidraw-fake-order", &path_value, Some(".TXT;.EXE")),
            Some(txt)
        );
        // Without the TXT extension allowed, the same lookup fails.
        assert_eq!(
            resolve_program_on_path("rapidraw-fake-order", &path_value, None),
            None
        );
        assert_eq!(
            resolve_program_on_path("rapidraw-missing-tool", &path_value, None),
            None
        );
    }

    // Path-like programs are never resolved through PATH.
    assert_eq!(
        resolve_program_on_path("subdir/tool", &format!("{}", base.display()), None),
        None
    );
    assert_eq!(
        resolve_program_on_path("", &format!("{}", base.display()), None),
        None
    );
}

/// A real spawn pins the interpreter through PATH resolution: a bare name
/// (`sh` on Unix, `cmd` on Windows) is resolved, validated as a file, and
/// the canonical binary is what runs — through an argument array only. The
/// response document comes from a `printf`/`type` payload so no shell
/// quoting is involved anywhere.
#[test]
fn workflow_security_runner_pins_bare_interpreter_from_path() {
    let base = temp_root("interpreter-pin");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let script = base.join("any-script.py");
    // `cmd /c` joins every trailing argument into one command line, so the
    // appended script path must stay benign: an empty file `type`
    // concatenates nothing after the prepared response.
    write(&script, "");
    let response_file = base.join("pinned-response.json");
    write(&response_file, r#"{"ok":true,"message":"pinned"}"#);

    #[cfg(unix)]
    let interpreter = rapidraw_lib::export_workflows::WorkflowInterpreter {
        language: rapidraw_lib::export_workflows::WorkflowLanguage::Python,
        program: "sh".to_string(),
        prefix_args: vec![
            "-c".to_string(),
            format!("printf '%s' \"$(cat '{}')\"", response_file.display()),
        ],
    };
    #[cfg(windows)]
    let interpreter = rapidraw_lib::export_workflows::WorkflowInterpreter {
        language: rapidraw_lib::export_workflows::WorkflowLanguage::Python,
        program: "cmd".to_string(),
        // `cmd /d /c type <file>` prints the prepared response verbatim;
        // no quotes are mangled because the path carries none.
        prefix_args: vec![
            "/d".to_string(),
            "/c".to_string(),
            format!("type {}", response_file.display()),
        ],
    };

    let roots = [workspace.to_path_buf()];
    let request = sample_request("run-pin", &workspace);
    let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let cancel = AtomicBool::new(false);
    let output = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect("PATH-pinned interpreter runs");
    assert!(output.response.ok, "{:?}", output.response);
    assert_eq!(output.response.message.as_deref(), Some("pinned"));
}

/// Interpreters that vanished or resolve to non-files between probe and
/// spawn fail with a typed spawn error instead of silently falling back to
/// another PATH entry or spawning a replacement.
#[test]
fn workflow_security_runner_rejects_replaced_or_missing_interpreters() {
    let base = temp_root("interpreter-replaced");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let script = base.join("script.py");
    write(&script, "# never launched\n");
    let request = sample_request("run-replaced", &workspace);
    let roots = [workspace.to_path_buf()];

    let deleted = base.join("deleted-interpreter.py");
    write(&deleted, "#!/bin/sh\n");
    std::fs::remove_file(&deleted).expect("delete interpreter");
    let missing = absolute_interpreter(&deleted);
    let spec = WorkflowRunSpec::new(&missing, &script, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let cancel = AtomicBool::new(false);
    let error = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect_err("deleted interpreter must not spawn");
    match error {
        WorkflowExecutionError::Spawn { reason } => {
            assert!(reason.contains("could not be resolved"), "{reason}");
        }
        other => panic!("expected Spawn, got {other:?}"),
    }

    // A directory where an interpreter should be is equally refused.
    let directory = base.join("directory-interpreter");
    std::fs::create_dir_all(&directory).expect("create directory");
    let dir_interpreter = absolute_interpreter(&directory);
    let spec = WorkflowRunSpec::new(&dir_interpreter, &script, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let error = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect_err("directory interpreter must not spawn");
    assert!(
        matches!(error, WorkflowExecutionError::Spawn { .. }),
        "{error:?}"
    );
}

/// The interpreter pin may canonicalize through symlinks (system pythons are
/// commonly symlinked); the resolved target runs normally. Unix-only because
/// creating a symlinked executable needs no privileges there.
#[cfg(unix)]
#[test]
fn workflow_security_runner_canonicalizes_symlinked_interpreters() {
    let base = temp_root("interpreter-symlink");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let script = base.join("script.py");
    write(&script, "# payload path only\n");

    use std::os::unix::fs::PermissionsExt;
    let target = base.join("real-interpreter.py");
    std::fs::write(
        &target,
        "#!/bin/sh\nprintf '%s' '{\"ok\":true,\"message\":\"via-symlink\"}'\n",
    )
    .expect("write target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let link = base.join("linked-interpreter.py");
    std::os::unix::fs::symlink(&target, &link).expect("symlink interpreter");

    let interpreter = fake_interpreter_for(&link);
    let request = sample_request("run-symlinked", &workspace);
    let roots = [workspace.to_path_buf()];
    let spec = WorkflowRunSpec::new(&interpreter, &link, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let cancel = AtomicBool::new(false);
    let output = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect("symlinked interpreter canonicalizes and runs");
    assert!(output.response.ok, "{:?}", output.response);
    assert_eq!(output.response.message.as_deref(), Some("via-symlink"));
}

// ---------------------------------------------------------------------------
// environment leakage
// ---------------------------------------------------------------------------

/// A secret placed in the parent environment never reaches the child (its
/// environment is cleared and allowlisted) and therefore can never come back
/// through captured output, warnings, or stderr excerpts.
#[test]
fn workflow_security_child_environment_and_results_never_leak_parent_secrets() {
    let base = temp_root("env-leak");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let capture = base.join("environment-dump.txt");

    let secret_name = "RAPIDRAW_SECURITY_TEST_SECRET";
    let secret_value = "leak-me-if-you-dare-060-9";
    unsafe { std::env::set_var(secret_name, secret_value) };

    #[cfg(unix)]
    let script = fake_executable(
        &base.join("bin"),
        "envdump",
        &format!(
            "env > '{capture}'\nprintf '%s' '{{\"ok\":true,\"warnings\":[\"environment dumped\"]}}'",
            capture = capture.display()
        ),
        "",
    );
    #[cfg(windows)]
    let script = fake_executable(
        &base.join("bin"),
        "envdump",
        "",
        &format!(
            "$names = Get-ChildItem env: | ForEach-Object {{ \"$($_.Name)=$($_.Value)\" }}\n\
             [IO.File]::WriteAllLines('{capture}', $names)\n\
             [IO.File]::AppendAllText('{capture}', \"\")\n\
             [Console]::Error.Write($names -join \"`n\")\n\
             [Console]::Out.Write('{{\"ok\":true,\"warnings\":[\"environment dumped\"]}}')",
            capture = capture.display(),
        ),
    );

    let interpreter = fake_interpreter_for(&script);
    let request = sample_request("run-env-leak", &workspace);
    let roots = [workspace.to_path_buf()];
    let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let cancel = AtomicBool::new(false);
    let output = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect("environment dump workflow runs");

    assert!(output.response.ok);
    assert!(
        !output.stderr.contains(secret_value),
        "secret must not appear in child stderr: {}",
        output.stderr
    );
    for warning in &output.response.warnings {
        assert!(
            !warning.contains(secret_value),
            "secret must not appear in warnings: {warning}"
        );
    }

    assert!(
        wait_for_file(&capture, Duration::from_secs(5)),
        "environment dump must be written"
    );
    let dumped = std::fs::read_to_string(&capture).expect("read dump");
    assert!(
        !dumped.contains(secret_value),
        "parent secret leaked into the child environment"
    );
    assert!(
        !dumped.contains(secret_name),
        "parent secret name leaked into the child environment"
    );
}

// ---------------------------------------------------------------------------
// process-tree cleanup on output exhaustion
// ---------------------------------------------------------------------------

/// A workflow that floods stdout past the byte limit while a grandchild
/// keeps running must be terminated tree-wide: the flood fails with the
/// typed output-limit error and the known descendant dies with the run.
#[test]
fn workflow_security_output_exhaustion_terminates_the_process_tree() {
    let base = temp_root("output-exhaustion");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let pidfile = base.join("grandchild.pid");
    let flood_bytes = rapidraw_lib::export_workflows::WORKFLOW_MAX_RESPONSE_BYTES + 1;

    #[cfg(unix)]
    let script = fake_executable(
        &base.join("bin"),
        "flood",
        &format!(
            "sleep 30 &\necho $! > '{pidfile}'\nhead -c {flood} /dev/zero | tr '\\0' x",
            pidfile = pidfile.display()
        ),
        "",
    );
    #[cfg(windows)]
    let script = fake_executable(
        &base.join("bin"),
        "flood",
        "",
        &format!(
            "$psi = New-Object System.Diagnostics.ProcessStartInfo\n\
             $psi.FileName = 'ping'\n\
             $psi.Arguments = '-n 30 127.0.0.1'\n\
             $psi.UseShellExecute = $false\n\
             $grandchild = [System.Diagnostics.Process]::Start($psi)\n\
             [IO.File]::WriteAllText('{pidfile}', \"$($grandchild.Id)\")\n\
             $s = 'x' * {flood_bytes}\n\
             [Console]::Out.Write($s)",
            pidfile = pidfile.display()
        ),
    );

    let interpreter = fake_interpreter_for(&script);
    let request = sample_request("run-flood-tree", &workspace);
    let roots = [workspace.to_path_buf()];
    let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let cancel = AtomicBool::new(false);
    let error = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect_err("stdout flood must fail");
    assert!(
        matches!(
            error,
            WorkflowExecutionError::OutputLimitExceeded {
                stream: rapidraw_lib::export_workflows::WorkflowOutputStream::Stdout
            }
        ),
        "{error:?}"
    );

    assert!(
        wait_for_file(&pidfile, Duration::from_secs(10)),
        "fixture must report its grandchild"
    );
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .expect("read pidfile")
        .trim()
        .parse()
        .expect("grandchild pid");
    assert!(
        wait_for_process_exit(pid),
        "grandchild process {pid} must die with the exhausted run"
    );
}

// ---------------------------------------------------------------------------
// packaging paths
// ---------------------------------------------------------------------------

/// The packaged examples shipped under `src-tauri/resources/workflows` must
/// discover as canonical, symlink-free, in-root bundled entries — the
/// packaged directory can never smuggle a path outside itself.
#[test]
fn workflow_security_packaged_bundled_examples_stay_inside_their_root() {
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/workflows");
    assert!(bundled.is_dir(), "packaged workflows directory must exist");
    let canonical_bundled = canonical_stripped(&bundled);

    let result = discover_root(&bundled);
    let ids: Vec<&str> = result.workflows.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "example-post-image",
            "example-receipt",
            "example-post-batch"
        ],
        "{ids:?}"
    );
    for workflow in &result.workflows {
        assert_eq!(
            workflow.source,
            rapidraw_lib::export_workflows::WorkflowSource::Bundled
        );
        assert!(workflow.selectable, "{:?}", workflow.diagnostics);
        assert!(workflow.diagnostics.is_empty());
        let script = PathBuf::from(&workflow.script_path);
        assert!(
            script.starts_with(&canonical_bundled),
            "{} must stay inside the packaged root",
            script.display()
        );
        let metadata = std::fs::symlink_metadata(&script).expect("script exists");
        assert!(
            !metadata.file_type().is_symlink(),
            "packaged scripts must not be symlinks: {}",
            script.display()
        );
        assert!(metadata.is_file());
        // And the spawn-time validator agrees on the real packaged files.
        validate_workflow_script_identity(&script, std::slice::from_ref(&canonical_bundled))
            .unwrap_or_else(|error| panic!("packaged script {script:?} must validate: {error}"));
    }
    // The scan of the packaged root reports nothing hostile; the only
    // diagnostic is the expected unresolved user root.
    assert_eq!(
        diagnostic_codes(&result),
        vec!["workflow.discovery.root.missing"]
    );
}

// ---------------------------------------------------------------------------
// shared fixtures
// ---------------------------------------------------------------------------

fn sample_request(
    run_id: &str,
    workspace: &Path,
) -> rapidraw_lib::export_workflows::WorkflowRequest {
    rapidraw_lib::export_workflows::WorkflowRequest {
        protocol_version: rapidraw_lib::export_workflows::WORKFLOW_PROTOCOL_VERSION,
        run_id: run_id.to_string(),
        workflow_id: "security-workflow".to_string(),
        phase: WorkflowPhase::PostBatch,
        source_path: None,
        exported_path: None,
        artifacts: Vec::new(),
        selected_items: Vec::new(),
        exported_items: vec![WorkflowItem {
            source_path: "raw/DSC_0001.NEF".to_string(),
            exported_path: Some("out/DSC_0001.jpg".to_string()),
            artifacts: Vec::new(),
            error: None,
        }],
        export_settings: export_settings(),
        index: None,
        total: 1,
        workspace_temp_directory: workspace.to_string_lossy().to_string(),
    }
}

fn quick_limits() -> rapidraw_lib::export_workflows::WorkflowRunLimits {
    rapidraw_lib::export_workflows::WorkflowRunLimits {
        timeout: Duration::from_secs(30),
        max_stdout_bytes: rapidraw_lib::export_workflows::WORKFLOW_MAX_RESPONSE_BYTES,
        max_stderr_bytes: rapidraw_lib::export_workflows::WORKFLOW_MAX_STDERR_BYTES,
    }
}

/// An interpreter candidate pinned to an absolute program path.
fn absolute_interpreter(path: &Path) -> rapidraw_lib::export_workflows::WorkflowInterpreter {
    rapidraw_lib::export_workflows::WorkflowInterpreter {
        language: rapidraw_lib::export_workflows::WorkflowLanguage::Python,
        program: path.to_string_lossy().to_string(),
        prefix_args: Vec::new(),
    }
}
