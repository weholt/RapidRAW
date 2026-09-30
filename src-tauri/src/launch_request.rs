use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Emitter;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEditSession {
    pub source: String,
    pub output: String,
    pub format: String,
    pub jpeg_quality: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessExportSession {
    pub source: String,
    pub output: String,
    pub format: String,
    pub quality: u8,
    pub keep_metadata: bool,
    pub adjustments_override: Option<String>,
    /// Requested workflow ids in command-line order. Empty means no workflows
    /// run: GUI last-used selections never apply to headless exports.
    pub workflow_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LaunchRequest {
    None,
    OpenFile(String),
    EditSession(ExternalEditSession),
    HeadlessExport(HeadlessExportSession),
    /// `--list-workflows`: print the discovered workflow registry and exit.
    ListWorkflows,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPayload {
    pub open_with_file: Option<String>,
    pub edit_session: Option<ExternalEditSession>,
}

pub fn parse_launch_args(args: &[String]) -> LaunchRequest {
    // Listing wins over every other mode wherever it appears: it is a pure
    // registry query that never opens a window and never starts an export.
    if args.iter().any(|arg| arg == "--list-workflows") {
        return LaunchRequest::ListWorkflows;
    }

    if args.first().map(|s| s.as_str()) == Some("export") {
        let mut iter = args.iter().skip(1);

        let mut source = String::new();
        let mut output = String::new();
        let mut format = String::from("jpeg");
        let mut quality = 90;
        let mut keep_metadata = false;
        let mut adjustments_override = None;
        let mut workflow_ids = Vec::new();

        if let Some(src) = iter.next()
            && !src.starts_with('-')
        {
            source = src.clone();
        }

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output" => {
                    if let Some(out) = iter.next() {
                        output = out.clone();
                    }
                }
                "--format" => {
                    if let Some(fmt) = iter.next() {
                        format = fmt.clone();
                    }
                }
                "--quality" => {
                    if let Some(q) = iter.next() {
                        quality = q.parse().unwrap_or(90);
                    }
                }
                "--keep-metadata" => keep_metadata = true,
                "--adjustments" => {
                    if let Some(adj) = iter.next() {
                        adjustments_override = Some(adj.clone());
                    }
                }
                // Repeatable: every occurrence selects one workflow, and the
                // command-line order defines the execution order. A missing
                // value is ignored, consistent with every other flag here.
                "--workflow" => {
                    if let Some(id) = iter.next() {
                        workflow_ids.push(id.clone());
                    }
                }
                _ => {}
            }
        }

        return LaunchRequest::HeadlessExport(HeadlessExportSession {
            source,
            output,
            format,
            quality,
            keep_metadata,
            adjustments_override,
            workflow_ids,
        });
    }

    let mut edit: Option<String> = None;
    let mut output: Option<String> = None;
    let mut format: Option<String> = None;
    let mut quality: Option<u8> = None;
    let mut plain: Option<String> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--edit" => edit = iter.next().cloned(),
            "--output" => output = iter.next().cloned(),
            "--format" => format = iter.next().cloned(),
            "--quality" => quality = iter.next().and_then(|q| q.parse().ok()),
            s if !s.starts_with('-') && plain.is_none() => plain = Some(s.to_string()),
            _ => {}
        }
    }

    match (edit, output) {
        (Some(source), Some(output)) => {
            let format = format.unwrap_or_else(|| {
                std::path::Path::new(&output)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_else(|| "jpg".to_string())
            });
            let format = match format.as_str() {
                "tif" => "tiff".to_string(),
                _ => format,
            };
            LaunchRequest::EditSession(ExternalEditSession {
                source,
                output,
                format,
                jpeg_quality: quality.unwrap_or(90),
            })
        }
        (Some(source), None) => LaunchRequest::OpenFile(source),
        _ => match plain {
            Some(path) => LaunchRequest::OpenFile(path),
            None => LaunchRequest::None,
        },
    }
}

fn handle_file_open(app_handle: &tauri::AppHandle, path: PathBuf) {
    if let Some(path_str) = path.to_str()
        && let Err(e) = app_handle.emit("open-with-file", path_str)
    {
        log::error!("Failed to emit open-with-file event: {}", e);
    }
}

pub fn emit_launch_request(app_handle: &tauri::AppHandle, request: LaunchRequest) {
    match request {
        LaunchRequest::EditSession(session) => {
            if let Err(e) = app_handle.emit("external-edit-session", &session) {
                log::error!("Failed to emit external-edit-session event: {}", e);
            }
        }
        LaunchRequest::OpenFile(path) => {
            handle_file_open(app_handle, PathBuf::from(path));
        }
        LaunchRequest::HeadlessExport(_) => {
            println!(
                "Error: Headless export cannot be attached to an already running GUI instance."
            );
        }
        LaunchRequest::ListWorkflows => {
            println!(
                "Error: --list-workflows cannot be attached to an already running GUI instance; run it as its own command."
            );
        }
        LaunchRequest::None => {}
    }
}

/// Exit status policy shared by the headless commands: success is 0, every
/// failure (missing source, unknown workflow id, export error, fail-policy
/// workflow failure) is 1 with the message printed to stderr.
pub fn headless_exit_code(result: &Result<(), String>) -> i32 {
    if result.is_ok() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> LaunchRequest {
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        parse_launch_args(&owned)
    }

    fn export_session(args: &[&str]) -> HeadlessExportSession {
        match parse(args) {
            LaunchRequest::HeadlessExport(session) => session,
            other => panic!("expected a headless export session, got {other:?}"),
        }
    }

    #[test]
    fn launch_parser_export_without_workflow_flags_selects_no_workflows() {
        let session = export_session(&[
            "export",
            "/photos",
            "--output",
            "/out",
            "--format",
            "png",
            "--quality",
            "80",
            "--keep-metadata",
            "--adjustments",
            "/preset.json",
        ]);
        assert_eq!(session.source, "/photos");
        assert_eq!(session.output, "/out");
        assert_eq!(session.format, "png");
        assert_eq!(session.quality, 80);
        assert!(session.keep_metadata);
        assert_eq!(
            session.adjustments_override.as_deref(),
            Some("/preset.json")
        );
        assert!(
            session.workflow_ids.is_empty(),
            "no workflows run unless explicitly requested"
        );
    }

    #[test]
    fn launch_parser_repeated_workflow_flags_preserve_command_line_order() {
        let session = export_session(&[
            "export",
            "/photos",
            "--output",
            "/out",
            "--workflow",
            "gamma",
            "--workflow",
            "alpha",
            "--workflow",
            "beta",
        ]);
        assert_eq!(
            session.workflow_ids,
            vec!["gamma".to_string(), "alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn launch_parser_workflow_flag_without_a_value_is_ignored_like_other_flags() {
        let session = export_session(&["export", "/photos", "--output", "/out", "--workflow"]);
        assert!(session.workflow_ids.is_empty());

        // A flag missing its value does not swallow a later --workflow either.
        let session = export_session(&["export", "/photos", "--output", "--workflow", "alpha"]);
        assert!(session.workflow_ids.is_empty());
    }

    #[test]
    fn launch_parser_list_workflows_wins_standalone_and_inside_export() {
        assert!(matches!(
            parse(&["--list-workflows"]),
            LaunchRequest::ListWorkflows
        ));
        assert!(matches!(
            parse(&["export", "/photos", "--output", "/out", "--list-workflows"]),
            LaunchRequest::ListWorkflows
        ));
    }

    #[test]
    fn launch_parser_plain_open_and_edit_sessions_are_unchanged() {
        assert_eq!(
            parse(&["/photos/DSC_0001.NEF"]),
            LaunchRequest::OpenFile("/photos/DSC_0001.NEF".to_string())
        );
        assert!(matches!(parse(&[]), LaunchRequest::None));

        match parse(&[
            "--edit",
            "/photos/DSC_0001.NEF",
            "--output",
            "/out/edited.jpg",
            "--quality",
            "95",
        ]) {
            LaunchRequest::EditSession(session) => {
                assert_eq!(session.source, "/photos/DSC_0001.NEF");
                assert_eq!(session.output, "/out/edited.jpg");
                assert_eq!(session.format, "jpg");
                assert_eq!(session.jpeg_quality, 95);
            }
            other => panic!("expected an edit session, got {other:?}"),
        }
    }

    #[test]
    fn launch_parser_headless_exit_code_maps_result_to_status() {
        assert_eq!(headless_exit_code(&Ok(())), 0);
        assert_eq!(
            headless_exit_code(&Err(
                "workflow 'x' is not in the workflow registry".to_string()
            )),
            1
        );
    }
}
