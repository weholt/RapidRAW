# Export workflow protocol v1 and threat model

Status: accepted. Normative Rust models live in `src-tauri/src/export_workflows.rs`; reviewed
against the Python and Node fixtures under `src-tauri/tests/fixtures/workflows/`.

Export workflows are trusted local automation scripts. A workflow is a single `.py` or `.js`
file discovered in the bundled resources `workflows` directory or `~/.rapidraw/workflows`.
Rust launches the file with the detected interpreter, passes one JSON request on stdin, and
reads one JSON response from stdout. Nothing else is part of the contract.

## Validity, defaults, and optional metadata

A standalone `.py` or `.js` file is a complete workflow; no manifest is required. Defaults are
derived from the file name and extension:

| Field          | Default                                                                                                                            |
| -------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| id             | file stem normalized to `^[a-z0-9][a-z0-9-]{0,63}$` (separators become `-`, runs collapse, truncated to 64, `workflow` when empty) |
| displayName    | stem with `-`/`_` replaced by single spaces                                                                                        |
| language       | `python` for `.py`, `javaScript` for `.js` (extension case-insensitive)                                                            |
| phase          | `postBatch`                                                                                                                        |
| order          | 100 (metadata range 0..=1000)                                                                                                      |
| timeoutSeconds | 60 (metadata range 1..=600)                                                                                                        |
| onError        | `warn`                                                                                                                             |

An optional adjacent sidecar `<script>.rapidraw.json` (script file name including extension,
e.g. `sharpen.py.rapidraw.json`) may override any of: `id`, `displayName`, `description`,
`phase`, `order`, `timeoutSeconds`, `onError` — all camelCase, all optional. Unknown sidecar
fields are ignored (forward compatibility). Out-of-contract values (bad id charset, order or
timeout out of range, display name over 100 chars, description over 500 chars, malformed JSON,
wrong types) produce typed `workflow.metadata.*` diagnostics; the entry is then listed as
invalid and is not selectable. Metadata enhances but never gates validity of a direct file.

Duplicate ids: within one root, the entry with the lexicographically smaller canonical script
path wins and a diagnostic reports the shadowed file; across roots, a user workflow with the
same stable id overrides the bundled one and the override is reported.

## Phases

- `postImage`: runs after one image's output file, metadata, timestamps, and any mask
  artifacts are fully written, while the batch continues exporting other images. The request
  carries that image's `sourcePath`, `exportedPath`, `artifacts`, `index`, and `total`;
  `selectedItems`/`exportedItems` are empty because the batch has not settled.
- `postBatch`: exactly one invocation after all image workers settle. The request carries the
  complete `selectedItems` and `exportedItems` lists (including failed and skipped entries);
  `sourcePath`, `exportedPath`, `artifacts`, and `index` are null/empty.

## Request (v1)

Fields: `protocolVersion` (u32, must be 1), `runId`, `workflowId`, `phase`, `sourcePath`,
`exportedPath`, `artifacts` (`{path, kind?}`), `selectedItems` and `exportedItems`
(`{sourcePath, exportedPath?, artifacts, error?}`), `exportSettings` subset
(`{fileFormat, jpegQuality, keepMetadata, stripGps}`), `index`, `total`,
`workspaceTempDirectory`. The serialized request is bounded to 1 MiB. Scripts must ignore
unknown fields; v1 additions are additive only.

`workspaceTempDirectory` is a per-run directory the script may use for scratch output. Paths
listed in `producedArtifactPaths` must be relative to it (or absolute and inside the export
root); the host canonicalizes and labels anything else `external`.

## Response (v1)

Fields: `ok` (required, bool), `message?`, `warnings?` (strings), `producedArtifactPaths?`.
Missing collections default to empty, so `{"ok": true}` is a valid minimal response. Stdout
must contain exactly one JSON document bounded to 256 KiB; stderr is diagnostic text only,
bounded to 64 KiB and truncated with a marker. Warnings are capped at 100 entries.

## Exit codes and distinct errors

Exit 0 with parseable JSON is the only success path. A response with `ok: false` is a
script-reported failure interpreted through `onError` (`warn` records a warning, `fail` fails
the affected image or batch) — it is not a protocol error. All host-side failures are distinct
typed outcomes (`WorkflowExecutionError`): `missingRuntime`, `spawn`, `malformedOutput`
(invalid JSON on stdout), `nonZeroExit` (with exit code), `outputLimitExceeded` (stdout or
stderr), `timedOut`, `cancelled`. Malformed output and non-zero exit are separate errors: a
script may exit 0 with invalid JSON, or exit non-zero after writing a partial document.

## Environment policy

The child process runs with a minimal environment: `PATH`, `HOME`/`USERPROFILE`, `TEMP`/`TMP`,
plus `SYSTEMROOT`/`COMSPEC` on Windows. The parent environment is never forwarded wholesale;
arbitrary secrets are not exposed by default. The working directory is the workspace temp
directory. The interpreter is launched directly with an argument array — never through
`cmd.exe`, `PowerShell`, `sh`, or a shell plugin — so spaces, quotes, Unicode, and shell
metacharacters in paths are passed literally. Workflow concurrency is bounded separately from
image export workers.

## Cancellation

Cancellation is host-driven and cooperative: unstarted workflow invocations are skipped,
running child process trees are terminated, and each affected invocation is recorded as
`cancelled`. v1 defines no in-band cancel signal. Timeout uses the effective
`timeoutSeconds` and produces `timedOut`.

## Threat model

Workflow scripts are trusted local code running with the full permissions of the user, exactly
as if the user ran `python script.py` themselves. There is no sandbox in v1: a malicious
workflow can read, modify, or delete files reachable by the user and can access the network.
Workflows downloaded from untrusted sources must not be installed. Mitigations are structural,
not protective: only the bundled resource directory and `~/.rapidraw/workflows` are scanned,
discovery never executes script code, workflows must be explicitly selected per export (none
run implicitly), execution uses bounded I/O, timeouts, a minimal environment, and no shell.
The UI surfaces a trust warning before first use of a newly discovered user workflow.

## Headless behavior

The headless CLI runs no workflows unless explicitly requested with repeatable `--workflow
<id>` flags, resolved through the same registry, protocol, runner, and error policies as the
GUI. Requested ids that are missing or unavailable fail before image export starts. GUI
last-used workflow selections never apply to headless exports.

## Versioning

`protocolVersion` is a major-only u32. Consumers reject any value other than 1 with an
actionable error naming the received and expected versions. Changes within v1 must be
additive (new optional request/response fields) and tolerated by existing parsers; a future
v2 is a breaking change negotiated by the version field.

## Review fixtures

`src-tauri/tests/fixtures/workflows/` contains one Python (`example_post_image.py`) and one
Node (`example_post_batch.js`) fixture implementing this contract with standard-library APIs
only, an example sidecar (`example_post_batch.js.rapidraw.json`), and canonical example
documents (`post-image-request.json`, `post-batch-request.json`, `response.json`) that are
validated against the Rust serde models by unit tests.
