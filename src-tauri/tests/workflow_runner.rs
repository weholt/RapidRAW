//! Integration tests for the bounded workflow subprocess runner
//! (`rapidraw-060.3`).
//!
//! Every test drives `CommandWorkflowRunner` through fake interpreters so the
//! full spawn/stdin/stdout/stderr/timeout/cancellation machinery runs for real
//! on the host platform:
//!
//! - Unix fakes are executable `#!/bin/sh` scripts launched directly (no
//!   shell, no shell=true).
//! - Windows fakes are `.ps1` scripts launched via `powershell -File`, again
//!   with an argument array only.
//!
//! The final test additionally runs the real protocol fixture scripts from
//! `tests/fixtures/workflows/` whenever a real Python or Node runtime is
//! installed, and skips with a note otherwise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rapidraw_lib::export_workflows::{
    CommandRuntimeProber, CommandWorkflowRunner, ProbePlatform, RuntimeCandidate, RuntimeProber,
    WORKFLOW_MAX_CONCURRENT_RUNS, WORKFLOW_MAX_RESPONSE_BYTES, WORKFLOW_MAX_STDERR_BYTES,
    WORKFLOW_PROTOCOL_VERSION, WorkflowExecutionError, WorkflowExportSettings, WorkflowInterpreter,
    WorkflowItem, WorkflowLanguage, WorkflowOutputStream, WorkflowPhase, WorkflowRequest,
    WorkflowRunLimits, WorkflowRunOutput, WorkflowRunSpec, WorkflowRunner,
    resolve_workflow_interpreter,
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/workflows");

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn sample_request(run_id: &str, phase: WorkflowPhase, workspace: &Path) -> WorkflowRequest {
    WorkflowRequest {
        protocol_version: WORKFLOW_PROTOCOL_VERSION,
        run_id: run_id.to_string(),
        workflow_id: "fake-workflow".to_string(),
        phase,
        source_path: Some("raw/DSC_0001.NEF".to_string()),
        exported_path: Some("export/DSC_0001.jpg".to_string()),
        artifacts: Vec::new(),
        selected_items: vec![WorkflowItem {
            source_path: "raw/DSC_0001.NEF".to_string(),
            exported_path: None,
            artifacts: Vec::new(),
            error: None,
        }],
        exported_items: vec![WorkflowItem {
            source_path: "raw/DSC_0001.NEF".to_string(),
            exported_path: Some("export/DSC_0001.jpg".to_string()),
            artifacts: Vec::new(),
            error: None,
        }],
        export_settings: WorkflowExportSettings {
            file_format: "jpeg".to_string(),
            jpeg_quality: 90,
            keep_metadata: true,
            strip_gps: false,
        },
        index: Some(0),
        total: 1,
        workspace_temp_directory: workspace.to_string_lossy().to_string(),
    }
}

fn write_bytes(path: &Path, contents: &[u8]) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    path.to_path_buf()
}

/// Writes one platform-specific fake workflow script from a file stem. On
/// Unix the `.py` script is executable and doubles as its own interpreter via
/// the shebang line; on Windows the `.ps1` script (PowerShell's `-File`
/// parameter only accepts `.ps1` files) is run through `powershell -NoProfile
/// -ExecutionPolicy Bypass -File <script>` with an argument array.
fn fake_workflow_script(dir: &Path, stem: &str, _sh_body: &str, _ps1_body: &str) -> PathBuf {
    #[cfg(unix)]
    let name = format!("{stem}.py");
    #[cfg(windows)]
    let name = format!("{stem}.ps1");
    let path = dir.join(name);
    std::fs::create_dir_all(dir).expect("create fake workflow dir");
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

/// The interpreter used to launch the fake workflow script: the script itself
/// on Unix, `powershell -File` on Windows. Neither goes through a shell.
fn interpreter_for(_script: &Path) -> WorkflowInterpreter {
    #[cfg(unix)]
    {
        WorkflowInterpreter {
            language: WorkflowLanguage::Python,
            program: _script.to_string_lossy().to_string(),
            prefix_args: Vec::new(),
        }
    }
    #[cfg(windows)]
    {
        WorkflowInterpreter {
            language: WorkflowLanguage::Python,
            program: "powershell".to_string(),
            prefix_args: vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
            ],
        }
    }
}

fn quick_limits() -> WorkflowRunLimits {
    WorkflowRunLimits {
        timeout: Duration::from_secs(30),
        max_stdout_bytes: WORKFLOW_MAX_RESPONSE_BYTES,
        max_stderr_bytes: WORKFLOW_MAX_STDERR_BYTES,
    }
}

fn run_fake(
    script: &Path,
    workspace: &Path,
    request: &WorkflowRequest,
    limits: WorkflowRunLimits,
) -> Result<WorkflowRunOutput, WorkflowExecutionError> {
    let interpreter = interpreter_for(script);
    let roots = [workspace.to_path_buf()];
    let spec = WorkflowRunSpec::new(&interpreter, script, request, workspace, &roots)
        .expect("fake spec builds");
    let spec = spec.with_limits(limits);
    let cancel = AtomicBool::new(false);
    CommandWorkflowRunner.run(&spec, &cancel)
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .trim()
        .to_string()
}

/// Canonical form without the Windows verbatim (`\\?\`) prefix, matching how
/// the runner reports artifact paths.
fn canonical(path: &Path) -> PathBuf {
    let text = path
        .canonicalize()
        .expect("canonicalize")
        .to_string_lossy()
        .to_string();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string())
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

fn grandchild_body(pidfile: &Path, child_keeps_running: bool) -> (String, String) {
    #[cfg(unix)]
    {
        let tail = if child_keeps_running {
            "sleep 30"
        } else {
            "exit 0"
        };
        (
            format!("sleep 30 &\necho $! > '{}'\n{}", pidfile.display(), tail),
            String::new(),
        )
    }
    #[cfg(windows)]
    {
        let tail = if child_keeps_running {
            "Start-Sleep -Seconds 30"
        } else {
            "exit 0"
        };
        (
            String::new(),
            format!(
                "$psi = New-Object System.Diagnostics.ProcessStartInfo\n\
                 $psi.FileName = 'ping'\n\
                 $psi.Arguments = '-n 30 127.0.0.1'\n\
                 $psi.UseShellExecute = $false\n\
                 $psi.RedirectStandardInput = $false\n\
                 $psi.RedirectStandardOutput = $false\n\
                 $psi.RedirectStandardError = $false\n\
                 $grandchild = [System.Diagnostics.Process]::Start($psi)\n\
                 [IO.File]::WriteAllText('{pid}', \"$($grandchild.Id)\")\n\
                 {tail}",
                pid = pidfile.display(),
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// success path
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_sends_valid_protocol_request_and_parses_response() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let capture = base.path().join("request-capture.json");

    let response_json = r#"{"ok":true,"message":"fake workflow done","warnings":[],"producedArtifactPaths":["receipt.json"]}"#;
    #[cfg(unix)]
    let (sh, ps1) = (
        format!(
            "cat > '{capture}'\nprintf '{{}}' > '{workspace}/receipt.json'\nprintf '%s' '{json}'",
            capture = capture.display(),
            workspace = workspace.display(),
            json = response_json,
        ),
        String::new(),
    );
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!(
            "$text = [Console]::In.ReadToEnd()\n\
             [IO.File]::WriteAllText('{capture}', $text)\n\
             [IO.File]::WriteAllText('{workspace}\\receipt.json', '{{}}')\n\
             [Console]::Out.Write('{json}')",
            capture = capture.display(),
            workspace = workspace.display(),
            json = response_json,
        ),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "fake_workflow", &sh, &ps1);

    let request = sample_request("run-success", WorkflowPhase::PostImage, &workspace);
    let output = run_fake(&script, &workspace, &request, quick_limits())
        .expect("fake workflow invocation succeeds");

    // The response is parsed, not merely surfaced as text.
    assert!(output.response.ok);
    assert_eq!(
        output.response.message.as_deref(),
        Some("fake workflow done")
    );
    assert_eq!(
        output.response.produced_artifact_paths,
        vec!["receipt.json".to_string()]
    );

    // The interpreter received a valid protocol v1 request on stdin.
    let captured: WorkflowRequest = serde_json::from_str(&read_trimmed(&capture))
        .expect("captured stdin is valid request JSON");
    assert_eq!(captured.protocol_version, WORKFLOW_PROTOCOL_VERSION);
    assert_eq!(captured.run_id, "run-success");
    assert_eq!(captured.phase, WorkflowPhase::PostImage);
    assert_eq!(captured.export_settings.jpeg_quality, 90);
    assert_eq!(
        captured.workspace_temp_directory,
        workspace.to_string_lossy().to_string()
    );

    // Internal produced artifacts are canonicalized and not labeled external.
    assert_eq!(output.produced_artifacts.len(), 1);
    let artifact = &output.produced_artifacts[0];
    assert_eq!(artifact.kind, None);
    assert_eq!(
        Path::new(&artifact.path),
        canonical(&workspace).join("receipt.json")
    );
}

#[test]
fn workflow_runner_passes_script_paths_literally_without_a_shell() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let capture = base.path().join("literal-capture.txt");

    // Spaces, quotes, dollar signs, semicolons, ampersands, and Unicode.
    // Characters illegal in Windows file names are avoided so one name works
    // on both platforms.
    let script_stem = "weird & name'é「算」'; $x";
    #[cfg(unix)]
    let (sh, ps1) = (
        format!(
            "printf '%s' \"$1\" > '{}'\nprintf '%s' '{{\"ok\":true}}'",
            capture.display()
        ),
        String::new(),
    );
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!(
            "[IO.File]::WriteAllText('{capture}', [System.IO.Path]::GetFileName($PSCommandPath))\n\
             [Console]::Out.Write('{{\"ok\":true}}')",
            capture = capture.display(),
        ),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), script_stem, &sh, &ps1)
        .canonicalize()
        .expect("canonicalize fake workflow");

    let request = sample_request("run-literal", WorkflowPhase::PostImage, &workspace);
    let output = run_fake(&script, &workspace, &request, quick_limits())
        .expect("script with hostile name runs");

    assert!(
        output.response.ok,
        "fixture must have executed: {:?}",
        output
    );

    // Unix: the interpreter reports the exact argv entry it received.
    #[cfg(unix)]
    {
        let received = read_trimmed(&capture);
        assert_eq!(received, script.to_string_lossy().to_string());
    }
    // Windows: powershell -File resolved exactly the named script.
    #[cfg(windows)]
    {
        let received = read_trimmed(&capture);
        assert_eq!(received, format!("{script_stem}.ps1"));
    }
}

#[test]
fn workflow_runner_script_reported_failure_is_not_an_execution_error() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let response_json =
        r#"{"ok":false,"message":"script said no","warnings":["w1"],"producedArtifactPaths":[]}"#;
    #[cfg(unix)]
    let (sh, ps1) = (format!("printf '%s' '{}'", response_json), String::new());
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!("[Console]::Out.Write('{}')", response_json),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "ok_false", &sh, &ps1);

    let request = sample_request("run-okfalse", WorkflowPhase::PostImage, &workspace);
    let output = run_fake(&script, &workspace, &request, quick_limits())
        .expect("ok:false is a script-reported outcome, not a host error");
    assert!(!output.response.ok);
    assert_eq!(output.response.message.as_deref(), Some("script said no"));
}

#[test]
fn workflow_runner_caps_script_warnings_at_the_protocol_limit() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let warnings: Vec<String> = (0..150).map(|i| format!("warning-{i}")).collect();
    let response_json = format!(
        "{{\"ok\":true,\"warnings\":{},\"producedArtifactPaths\":[]}}",
        serde_json::to_string(&warnings).expect("serialize warnings")
    );
    #[cfg(unix)]
    let (sh, ps1) = (format!("printf '%s' '{}'", response_json), String::new());
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!("[Console]::Out.Write('{}')", response_json),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "chatty", &sh, &ps1);

    let request = sample_request("run-chatty", WorkflowPhase::PostImage, &workspace);
    let output =
        run_fake(&script, &workspace, &request, quick_limits()).expect("chatty workflow succeeds");
    assert_eq!(output.response.warnings.len(), 100);
    assert_eq!(output.response.warnings[0], "warning-0");
    assert_eq!(output.response.warnings[99], "warning-99");
}

// ---------------------------------------------------------------------------
// typed failure modes
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_reports_malformed_stdout_json() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    #[cfg(unix)]
    let (sh, ps1) = ("printf 'not-json {'".to_string(), String::new());
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        "[Console]::Out.Write('not-json {')".to_string(),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "garbage", &sh, &ps1);

    let request = sample_request("run-malformed", WorkflowPhase::PostImage, &workspace);
    let error = run_fake(&script, &workspace, &request, quick_limits())
        .expect_err("malformed stdout must fail");
    match error {
        WorkflowExecutionError::MalformedOutput { reason } => {
            assert!(reason.contains("not a valid"), "{reason}");
        }
        other => panic!("expected MalformedOutput, got {other:?}"),
    }
}

#[test]
fn workflow_runner_reports_nonzero_exit_code_even_with_valid_json() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    #[cfg(unix)]
    let (sh, ps1) = (
        "printf '%s' '{\"ok\":true}'\nexit 3".to_string(),
        String::new(),
    );
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        "[Console]::Out.Write('{\"ok\":true}')\nexit 3".to_string(),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "fails", &sh, &ps1);

    let request = sample_request("run-nonzero", WorkflowPhase::PostImage, &workspace);
    let error = run_fake(&script, &workspace, &request, quick_limits())
        .expect_err("non-zero exit must fail");
    assert_eq!(error, WorkflowExecutionError::NonZeroExit { code: 3 });
}

#[test]
fn workflow_runner_enforces_the_stdout_byte_limit() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let flood = WORKFLOW_MAX_RESPONSE_BYTES + 1;
    #[cfg(unix)]
    let (sh, ps1) = (
        format!("head -c {flood} /dev/zero | tr '\\0' x"),
        String::new(),
    );
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!("$s = 'x' * {flood}\n[Console]::Out.Write($s)"),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "flood", &sh, &ps1);

    let request = sample_request("run-flood", WorkflowPhase::PostImage, &workspace);
    let error = run_fake(&script, &workspace, &request, quick_limits())
        .expect_err("stdout flood must fail");
    assert_eq!(
        error,
        WorkflowExecutionError::OutputLimitExceeded {
            stream: WorkflowOutputStream::Stdout,
        }
    );
}

#[test]
fn workflow_runner_enforces_the_stderr_byte_limit() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");

    let flood = WORKFLOW_MAX_STDERR_BYTES + 1;
    #[cfg(unix)]
    let (sh, ps1) = (
        format!("head -c {flood} /dev/zero | tr '\\0' e >&2\nprintf '%s' '{{\"ok\":true}}'"),
        String::new(),
    );
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!(
            "$s = 'e' * {flood}\n[Console]::Error.Write($s)\n[Console]::Out.Write('{{\"ok\":true}}')"
        ),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "noisy", &sh, &ps1);

    let request = sample_request("run-noisy", WorkflowPhase::PostImage, &workspace);
    let error = run_fake(&script, &workspace, &request, quick_limits())
        .expect_err("stderr flood must fail");
    assert_eq!(
        error,
        WorkflowExecutionError::OutputLimitExceeded {
            stream: WorkflowOutputStream::Stderr,
        }
    );
}

#[test]
fn workflow_runner_reports_missing_runtime_before_spawning() {
    #[derive(Default)]
    struct NeverProber {
        attempts: Mutex<Vec<String>>,
    }
    impl RuntimeProber for NeverProber {
        fn probe(&self, candidate: &RuntimeCandidate) -> Result<String, String> {
            self.attempts.lock().unwrap().push(candidate.display());
            Err(format!("{} is not available", candidate.program))
        }
    }
    let prober = NeverProber::default();

    let error =
        resolve_workflow_interpreter(WorkflowLanguage::Python, ProbePlatform::Unix, &prober)
            .expect_err("no candidate is available");
    match error {
        WorkflowExecutionError::MissingRuntime { language, reason } => {
            assert_eq!(language, WorkflowLanguage::Python);
            assert!(reason.contains("python3"), "{reason}");
            assert!(reason.contains("python"), "{reason}");
        }
        other => panic!("expected MissingRuntime, got {other:?}"),
    }
    // Probes used argument arrays, never a command string.
    let attempts = prober.attempts.lock().unwrap().clone();
    assert_eq!(
        attempts,
        vec![
            "python3 --version".to_string(),
            "python --version".to_string()
        ]
    );
}

#[test]
fn workflow_runner_reports_child_spawn_failure() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let script = write_bytes(
        &base.path().join("workflows").join("never_launched.py"),
        b"printf '%s' '{\"ok\":true}'",
    );

    let interpreter = WorkflowInterpreter {
        language: WorkflowLanguage::Python,
        program: base
            .path()
            .join("definitely-missing-interpreter-binary")
            .to_string_lossy()
            .to_string(),
        prefix_args: Vec::new(),
    };
    let request = sample_request("run-spawn", WorkflowPhase::PostImage, &workspace);
    let roots = [workspace.to_path_buf()];
    let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
        .expect("spec builds");
    let cancel = AtomicBool::new(false);

    let error = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect_err("missing interpreter binary must fail");
    match error {
        WorkflowExecutionError::Spawn { reason } => {
            assert!(!reason.is_empty());
        }
        other => panic!("expected Spawn, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// timeout and cancellation terminate the whole process tree
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_timeout_kills_the_entire_process_tree() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let pidfile = base.path().join("grandchild.pid");

    let (sh, ps1) = grandchild_body(&pidfile, true);
    let script = fake_workflow_script(&base.path().join("workflows"), "hangs", &sh, &ps1);

    let request = sample_request("run-timeout", WorkflowPhase::PostImage, &workspace);
    let limits = WorkflowRunLimits {
        timeout: Duration::from_millis(800),
        ..quick_limits()
    };
    let error = run_fake(&script, &workspace, &request, limits)
        .expect_err("hanging workflow must time out");
    assert_eq!(error, WorkflowExecutionError::TimedOut);

    assert!(
        wait_for_file(&pidfile, Duration::from_secs(10)),
        "fixture must report its grandchild"
    );
    let pid: u32 = read_trimmed(&pidfile).parse().expect("grandchild pid");
    assert!(
        wait_for_process_exit(pid),
        "grandchild process {pid} must be terminated with the timed-out workflow"
    );
}

#[test]
fn workflow_runner_cancellation_kills_the_process_tree_after_child_exit() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let pidfile = base.path().join("grandchild.pid");

    // The direct child exits immediately; the grandchild it spawned keeps
    // running and holds the stdout pipe, so only a tree-wide termination can
    // end the invocation. This exercises the job object (Windows) and process
    // group (Unix) kill paths for already-exited leaders.
    let (sh, ps1) = grandchild_body(&pidfile, false);
    let script = fake_workflow_script(&base.path().join("workflows"), "spawns", &sh, &ps1);

    let interpreter = interpreter_for(&script);
    let request = sample_request("run-cancel", WorkflowPhase::PostImage, &workspace);
    let roots = vec![workspace.clone()];
    let workspace_dir = workspace.clone();
    let script_path = script.clone();
    let cancel = Arc::new(AtomicBool::new(false));

    let worker = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let spec =
                WorkflowRunSpec::new(&interpreter, &script_path, &request, &workspace_dir, &roots)
                    .expect("spec builds")
                    .with_limits(WorkflowRunLimits {
                        timeout: Duration::from_secs(30),
                        ..quick_limits()
                    });
            let runner = CommandWorkflowRunner;
            runner.run(&spec, &cancel)
        })
    };

    assert!(
        wait_for_file(&pidfile, Duration::from_secs(15)),
        "fixture must spawn its grandchild before cancelling"
    );
    cancel.store(true, Ordering::Relaxed);

    let error = worker
        .join()
        .expect("runner thread must not panic")
        .expect_err("cancelled run fails");
    assert_eq!(error, WorkflowExecutionError::Cancelled);

    let pid: u32 = read_trimmed(&pidfile).parse().expect("grandchild pid");
    assert!(
        wait_for_process_exit(pid),
        "grandchild process {pid} must be terminated when the run is cancelled"
    );
}

// ---------------------------------------------------------------------------
// environment policy
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_launches_children_with_only_allowlisted_environment() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let capture = base.path().join("environment.txt");

    // A parent-environment secret that must never reach the child. Setting it
    // is process-global, which is harmless for the other tests in this
    // binary: their assertions never inspect the parent environment.
    let secret = "RAPIDRAW_RUNNER_TEST_SECRET";
    unsafe { std::env::set_var(secret, "leak-me-if-you-dare") };

    #[cfg(unix)]
    let allowed = ["PATH", "TEMP", "TMP", "HOME"];
    #[cfg(windows)]
    let allowed = [
        "PATH",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "SYSTEMROOT",
        "COMSPEC",
    ];
    // Interpreters add a few of their own variables at startup; the host must
    // not be blamed for those.
    #[cfg(unix)]
    let interpreter_added = ["PWD", "SHLVL", "OLDPWD", "_"];
    #[cfg(windows)]
    let interpreter_added = ["PATHEXT", "PSMODULEPATH", "PSEXECUTIONPOLICYPREFERENCE"];

    #[cfg(unix)]
    let (sh, ps1) = (
        format!(
            "env > '{}'\nprintf '%s' '{{\"ok\":true}}'",
            capture.display()
        ),
        String::new(),
    );
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!(
            "$names = @(Get-ChildItem env: | ForEach-Object {{ $_.Name }})\n\
             [IO.File]::WriteAllLines('{capture}', $names)\n\
             [Console]::Out.Write('{{\"ok\":true}}')",
            capture = capture.display(),
        ),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "envdump", &sh, &ps1);

    let request = sample_request("run-env", WorkflowPhase::PostImage, &workspace);
    let output = run_fake(&script, &workspace, &request, quick_limits())
        .expect("environment dump workflow succeeds");
    assert!(output.response.ok);

    let names: Vec<String> = read_trimmed(&capture)
        .lines()
        .map(|line| line.split('=').next().unwrap_or(line).trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    assert!(!names.is_empty(), "child environment must be observable");

    let normalize = |name: &str| name.to_ascii_uppercase();
    for name in &names {
        let upper = normalize(name);
        assert!(
            allowed.iter().any(|a| normalize(a) == upper)
                || interpreter_added.iter().any(|a| normalize(a) == upper),
            "environment variable {name} was forwarded but is not allowlisted"
        );
    }
    assert!(
        names.iter().any(|n| normalize(n) == "PATH"),
        "PATH must be forwarded: {names:?}"
    );
    assert!(
        names.iter().all(|n| normalize(n) != normalize(secret)),
        "parent secret {secret} leaked into the child environment"
    );
}

// ---------------------------------------------------------------------------
// artifact canonicalization
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_labels_artifacts_outside_the_roots_as_external() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let export_root = base.path().join("export");
    std::fs::create_dir_all(&export_root).expect("create export root");

    write_bytes(&workspace.join("receipt.json"), b"{}");
    write_bytes(&export_root.join("sidecar.txt"), b"{}");
    write_bytes(&base.path().join("escape.txt"), b"{}");

    let outside = if cfg!(windows) {
        r"C:\Windows\win.ini".to_string()
    } else {
        "/etc/hostname".to_string()
    };
    let paths_json = serde_json::json!([
        "receipt.json",
        export_root.join("sidecar.txt").to_string_lossy(),
        outside,
        "../escape.txt",
        "missing.json",
    ])
    .to_string();
    let response_json = format!("{{\"ok\":true,\"producedArtifactPaths\":{paths_json}}}");

    #[cfg(unix)]
    let (sh, ps1) = (format!("printf '%s' '{}'", response_json), String::new());
    #[cfg(windows)]
    let (sh, ps1) = (
        String::new(),
        format!("[Console]::Out.Write('{}')", response_json),
    );
    let script = fake_workflow_script(&base.path().join("workflows"), "artifacts", &sh, &ps1);

    let request = sample_request("run-artifacts", WorkflowPhase::PostImage, &workspace);
    // The export root is passed as an additional allowed root besides the
    // working directory.
    let interpreter = interpreter_for(&script);
    let roots = [workspace.to_path_buf(), export_root.clone()];
    let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
        .expect("spec builds")
        .with_limits(quick_limits());
    let cancel = AtomicBool::new(false);
    let output = CommandWorkflowRunner
        .run(&spec, &cancel)
        .expect("artifact workflow succeeds");

    let kinds: Vec<Option<&str>> = output
        .produced_artifacts
        .iter()
        .map(|artifact| artifact.kind.as_deref())
        .collect();
    assert_eq!(
        kinds,
        vec![
            None,
            None,
            Some("external"),
            Some("external"),
            Some("external"),
        ],
        "{:?}",
        output.produced_artifacts
    );

    // Internal artifacts are canonical absolute paths.
    assert_eq!(
        Path::new(&output.produced_artifacts[0].path),
        canonical(&workspace).join("receipt.json")
    );
    assert_eq!(
        Path::new(&output.produced_artifacts[1].path),
        canonical(&export_root).join("sidecar.txt")
    );
    assert!(
        output.produced_artifacts[4].path.ends_with("missing.json"),
        "unresolvable artifacts keep their raw path: {:?}",
        output.produced_artifacts[4]
    );
}

// ---------------------------------------------------------------------------
// concurrency gate
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_concurrency_is_bounded_separately_from_export_workers() {
    let gate =
        rapidraw_lib::export_workflows::WorkflowConcurrencyGate::new(WORKFLOW_MAX_CONCURRENT_RUNS);
    assert_eq!(gate.limit(), WORKFLOW_MAX_CONCURRENT_RUNS);

    let mut permits = Vec::new();
    for _ in 0..WORKFLOW_MAX_CONCURRENT_RUNS {
        permits.push(gate.try_acquire().expect("permit within the limit"));
    }
    assert!(
        gate.try_acquire().is_none(),
        "workflow runs beyond the limit must be rejected"
    );
    drop(permits.pop());
    assert!(
        gate.try_acquire().is_some(),
        "released permits return to the pool"
    );
}

// ---------------------------------------------------------------------------
// real interpreters with the protocol fixtures (skips when unavailable)
// ---------------------------------------------------------------------------

#[test]
fn workflow_runner_runs_protocol_fixtures_with_real_interpreters_when_available() {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let runner = CommandWorkflowRunner;
    let cancel = AtomicBool::new(false);
    let prober = CommandRuntimeProber {
        timeout: Duration::from_secs(2),
    };
    let platform = if cfg!(windows) {
        ProbePlatform::Windows
    } else {
        ProbePlatform::Unix
    };

    let python = resolve_workflow_interpreter(WorkflowLanguage::Python, platform, &prober);
    match python {
        Ok(interpreter) => {
            let script = Path::new(FIXTURES).join("example_post_image.py");
            let request = sample_request("run-fixture-py", WorkflowPhase::PostImage, &workspace);
            let roots = [workspace.to_path_buf()];
            let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
                .expect("python fixture spec builds")
                .with_limits(WorkflowRunLimits {
                    timeout: Duration::from_secs(30),
                    ..quick_limits()
                });
            let output = runner
                .run(&spec, &cancel)
                .unwrap_or_else(|error| panic!("real python fixture run failed: {error}"));
            assert!(output.response.ok, "{:?}", output.response);
            assert_eq!(output.produced_artifacts.len(), 1);
            assert_eq!(output.produced_artifacts[0].kind, None);
            let receipt = canonical(&workspace).join("receipt.json");
            assert_eq!(Path::new(&output.produced_artifacts[0].path), receipt);
        }
        Err(error) => eprintln!("skipping real python fixture run: {error}"),
    }

    let node = resolve_workflow_interpreter(WorkflowLanguage::JavaScript, platform, &prober);
    match node {
        Ok(interpreter) => {
            let script = Path::new(FIXTURES).join("example_post_batch.js");
            let request = sample_request("run-fixture-js", WorkflowPhase::PostBatch, &workspace);
            let roots = [workspace.to_path_buf()];
            let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
                .expect("node fixture spec builds")
                .with_limits(WorkflowRunLimits {
                    timeout: Duration::from_secs(30),
                    ..quick_limits()
                });
            let output = runner
                .run(&spec, &cancel)
                .unwrap_or_else(|error| panic!("real node fixture run failed: {error}"));
            assert!(output.response.ok, "{:?}", output.response);
            assert_eq!(output.produced_artifacts.len(), 1);
            assert_eq!(output.produced_artifacts[0].kind, None);
            let receipt = canonical(&workspace).join("receipt.json");
            assert_eq!(Path::new(&output.produced_artifacts[0].path), receipt);
        }
        Err(error) => eprintln!("skipping real node fixture run: {error}"),
    }
}

/// The packaged examples shipped under `src-tauri/resources/workflows`
/// (rapidraw-060.7) must run through the real runner exactly like the review
/// fixtures whenever an interpreter is available: validate the request, write
/// one sidecar receipt into the workspace temp directory, and report it as a
/// produced artifact. Skips with a note when the runtime is unavailable.
#[test]
fn workflow_runner_runs_packaged_bundled_examples_when_interpreters_available() {
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/workflows");
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let runner = CommandWorkflowRunner;
    let cancel = AtomicBool::new(false);
    let prober = CommandRuntimeProber {
        timeout: Duration::from_secs(2),
    };
    let platform = if cfg!(windows) {
        ProbePlatform::Windows
    } else {
        ProbePlatform::Unix
    };

    let python = resolve_workflow_interpreter(WorkflowLanguage::Python, platform, &prober);
    match &python {
        Ok(interpreter) => {
            let script = bundled.join("example_post_image.py");
            let request = sample_request("bundled-py", WorkflowPhase::PostImage, &workspace);
            let roots = [workspace.to_path_buf()];
            let spec = WorkflowRunSpec::new(interpreter, &script, &request, &workspace, &roots)
                .expect("bundled python example spec builds")
                .with_limits(WorkflowRunLimits {
                    timeout: Duration::from_secs(30),
                    ..quick_limits()
                });
            let output = runner
                .run(&spec, &cancel)
                .unwrap_or_else(|error| panic!("bundled python example run failed: {error}"));
            assert!(output.response.ok, "{:?}", output.response);
            assert_eq!(output.produced_artifacts.len(), 1);
            assert_eq!(output.produced_artifacts[0].kind, None);
            let receipt = canonical(&workspace).join("receipt.json");
            assert_eq!(Path::new(&output.produced_artifacts[0].path), receipt);
            let body: serde_json::Value =
                serde_json::from_str(&read_trimmed(&receipt)).expect("receipt is valid JSON");
            assert_eq!(body["runId"], "bundled-py");
            assert_eq!(body["workflowId"], "fake-workflow");
            assert_eq!(body["sourcePath"], "raw/DSC_0001.NEF");
        }
        Err(error) => eprintln!("skipping bundled python example run: {error}"),
    }

    let node = resolve_workflow_interpreter(WorkflowLanguage::JavaScript, platform, &prober);
    match &node {
        Ok(interpreter) => {
            let script = bundled.join("example_post_batch.js");
            let request = sample_request("bundled-js", WorkflowPhase::PostBatch, &workspace);
            let roots = [workspace.to_path_buf()];
            let spec = WorkflowRunSpec::new(interpreter, &script, &request, &workspace, &roots)
                .expect("bundled node example spec builds")
                .with_limits(WorkflowRunLimits {
                    timeout: Duration::from_secs(30),
                    ..quick_limits()
                });
            let output = runner
                .run(&spec, &cancel)
                .unwrap_or_else(|error| panic!("bundled node example run failed: {error}"));
            assert!(output.response.ok, "{:?}", output.response);
            assert_eq!(output.produced_artifacts.len(), 1);
            assert_eq!(output.produced_artifacts[0].kind, None);
            let receipt = canonical(&workspace).join("receipt.json");
            assert_eq!(Path::new(&output.produced_artifacts[0].path), receipt);
            let body: serde_json::Value =
                serde_json::from_str(&read_trimmed(&receipt)).expect("receipt is valid JSON");
            assert_eq!(body["runId"], "bundled-js");
            assert_eq!(body["exportedCount"], 1);
            assert_eq!(body["selectedCount"], 1);
        }
        Err(error) => eprintln!("skipping bundled node example run: {error}"),
    }

    if let Ok(interpreter) = &python {
        let script = bundled.join("example_receipt.py");
        let request = sample_request("bundled-receipt", WorkflowPhase::PostBatch, &workspace);
        let roots = [workspace.to_path_buf()];
        let spec = WorkflowRunSpec::new(interpreter, &script, &request, &workspace, &roots)
            .expect("bundled direct-file example spec builds")
            .with_limits(WorkflowRunLimits {
                timeout: Duration::from_secs(30),
                ..quick_limits()
            });
        let output = runner
            .run(&spec, &cancel)
            .unwrap_or_else(|error| panic!("bundled direct-file example run failed: {error}"));
        assert!(output.response.ok, "{:?}", output.response);
        assert_eq!(output.produced_artifacts.len(), 1);
        assert_eq!(output.produced_artifacts[0].kind, None);
        let receipt = canonical(&workspace).join("receipt.json");
        assert_eq!(Path::new(&output.produced_artifacts[0].path), receipt);
        let body: serde_json::Value =
            serde_json::from_str(&read_trimmed(&receipt)).expect("receipt is valid JSON");
        assert_eq!(body["runId"], "bundled-receipt");
        assert_eq!(body["exportedCount"], 1);
        assert_eq!(body["selectedCount"], 1);
    }
}

/// Cancels a real interpreter fixture that is mid-sleep, proving the cancel
/// control reaches active Python and Node subprocesses (and their trees) and
/// yields `Cancelled`, not a timeout. Skips with a note when the runtime is
/// unavailable.
fn assert_cancel_kills_sleeping_fixture(
    language: WorkflowLanguage,
    script_name: &str,
    run_id: &str,
    phase: WorkflowPhase,
) {
    let base = tempfile::tempdir().expect("base tempdir");
    let workspace = base.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let prober = CommandRuntimeProber {
        timeout: Duration::from_secs(2),
    };
    let platform = if cfg!(windows) {
        ProbePlatform::Windows
    } else {
        ProbePlatform::Unix
    };
    let interpreter = match resolve_workflow_interpreter(language, platform, &prober) {
        Ok(interpreter) => interpreter,
        Err(error) => {
            eprintln!("skipping sleeping {script_name} cancellation: {error}");
            return;
        }
    };

    let script = Path::new(FIXTURES).join(script_name);
    let request = sample_request(run_id, phase, &workspace);
    let roots = vec![workspace.clone()];
    let cancel = Arc::new(AtomicBool::new(false));
    let marker = workspace.join("started.marker");

    let worker = {
        let cancel = Arc::clone(&cancel);
        let workspace = workspace.clone();
        std::thread::spawn(move || {
            let spec = WorkflowRunSpec::new(&interpreter, &script, &request, &workspace, &roots)
                .expect("sleeping fixture spec builds")
                .with_limits(WorkflowRunLimits {
                    timeout: Duration::from_secs(30),
                    ..quick_limits()
                });
            CommandWorkflowRunner.run(&spec, &cancel)
        })
    };

    assert!(
        wait_for_file(&marker, Duration::from_secs(15)),
        "sleeping fixture must signal it is mid-run before cancellation"
    );
    let started_run = read_trimmed(&marker);
    assert_eq!(started_run, run_id, "fixture received the request on stdin");

    cancel.store(true, Ordering::Relaxed);
    let error = worker
        .join()
        .expect("runner thread must not panic")
        .expect_err("cancelled sleeping fixture fails");
    assert_eq!(
        error,
        WorkflowExecutionError::Cancelled,
        "cancellation must beat the 30s timeout for a mid-sleep subprocess"
    );
}

#[test]
fn workflow_runner_cancellation_terminates_sleeping_python_fixture() {
    assert_cancel_kills_sleeping_fixture(
        WorkflowLanguage::Python,
        "sleeping.py",
        "run-cancel-py",
        WorkflowPhase::PostImage,
    );
}

#[test]
fn workflow_runner_cancellation_terminates_sleeping_node_fixture() {
    assert_cancel_kills_sleeping_fixture(
        WorkflowLanguage::JavaScript,
        "sleeping.js",
        "run-cancel-js",
        WorkflowPhase::PostBatch,
    );
}
