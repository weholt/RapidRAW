# Export workflows

Export workflows are trusted local automation scripts that run as part of an export. A workflow
is a single `.py` or `.js` file; Rust launches it with the detected interpreter, passes one JSON
request on stdin, and reads one JSON response from stdout. The normative protocol, threat model,
and error semantics live in [the workflow protocol decision](decisions/export-workflow-protocol.md).

**Workflows are trusted local code.** They run with your full user permissions — they can read,
modify, or delete any file you can and can access the network. Never install a workflow from a
source you do not trust. RapidRAW does not sandbox workflow execution; discovery never executes
script code, no workflow runs unless explicitly selected for an export, the first workflow export
requires a one-time trust confirmation, and bundled examples are never auto-selected.

## First-run trust consent

The first export that selects at least one workflow shows a trust warning before anything runs:
it spells out the full-permissions/no-sandbox model above and must be accepted explicitly.
Acceptance is recorded per installation (versioned, with a timestamp, in the app's local storage)
and later workflow exports proceed without the prompt. Declining cancels the export; deselect the
workflows to export without them. Headless exports are unaffected: they only run workflows
requested through the explicit `--workflow` flag.

## Where workflows are discovered

- **Bundled:** the `resources/workflows` directory shipped with the application (Windows and
  macOS installers, Linux packages, and the Flatpak `/app/lib/RapidRAW/resources/workflows`).
  It contains the `example_receipt.py`, `example_post_image.py`, and `example_post_batch.js`
  examples described below.
- **User:** `~/.rapidraw/workflows` (create it if it does not exist). A user workflow with the
  same stable id as a bundled one overrides it, and the override is reported.

Only these two directories are scanned, and only their top level. The export panel's workflow
section lists what discovery found — press refresh to pick up files added or removed while the
panel is open, without restarting the app. Android hides the section because it does not ship
interpreters.

## Direct-file defaults

A standalone `.py` or `.js` file is a complete workflow; no manifest and no third-party packages
are required. Defaults are derived from the file name and extension:

| Field          | Default                                                                         |
| -------------- | ------------------------------------------------------------------------------- |
| id             | file stem normalized to lowercase `a-z0-9-` (e.g. `My Script.py` → `my-script`) |
| displayName    | stem with `-`/`_` replaced by spaces                                            |
| language       | `python` for `.py`, `javaScript` for `.js` (extension case-insensitive)         |
| phase          | `postBatch`                                                                     |
| order          | 100 (metadata range 0–1000)                                                     |
| timeoutSeconds | 60 (metadata range 1–600)                                                       |
| onError        | `warn`                                                                          |

## Optional metadata sidecar

Any workflow may carry an adjacent `<script>.rapidraw.json` sidecar (the script file name
including its extension, e.g. `sharpen.py.rapidraw.json`) that overrides `id`, `displayName`,
`description`, `phase`, `order`, `timeoutSeconds`, and `onError` — all optional, camelCase.
Unknown fields are ignored; out-of-contract values mark the entry invalid and unselectable with
an actionable reason.

## Phases

- `postImage`: runs after one image's output file, metadata, timestamps, and mask artifacts are
  fully written, while the batch continues. The request carries that image's `sourcePath`,
  `exportedPath`, `artifacts`, `index`, and `total`.
- `postBatch`: exactly one invocation after all images settle. The request carries the complete
  `selectedItems` and `exportedItems` lists; per-image fields are null/empty.

Selected workflows run in your chosen order. `onError: warn` (default) records a warning and
continues; `onError: fail` fails the affected image or batch. Each invocation has a bounded
timeout, and cancelling an export terminates running workflow processes.

## Request and response

The host writes one JSON request (bounded to 1 MiB) to stdin: `protocolVersion` (currently 1),
`runId`, `workflowId`, `phase`, per-image or per-batch item fields, an `exportSettings` subset,
`index`, `total`, and `workspaceTempDirectory` — a per-run scratch directory. The script must
write exactly one JSON response document to stdout (bounded to 256 KiB): `{"ok": true}` at
minimum, optionally `message`, `warnings`, and `producedArtifactPaths` relative to the workspace
temp directory. stderr is captured as bounded diagnostic text. Canonical example documents live
in `src-tauri/tests/fixtures/workflows/`.

## Shipped examples

The bundled directory includes three minimal, deterministic, offline examples that use only the
Python standard library or Node core modules. Each validates the request, writes a `receipt.json`
sidecar receipt into the workspace temp directory, and reports it as a produced artifact:

- `example_receipt.py` — bare direct file demonstrating every derived default (runs `postBatch`).
- `example_post_image.py` + `example_post_image.py.rapidraw.json` — a minimal sidecar containing
  only `{"phase": "postImage"}`; every other default is kept.
- `example_post_batch.js` + `example_post_batch.js.rapidraw.json` — a full metadata sidecar
  overriding id, display name, description, order, timeout, and error policy.

Copy any of them into `~/.rapidraw/workflows`, adjust the ids, and select them in the export
panel's workflow section to see them run.

## Headless usage

Headless exports use the same discovery registry, protocol, runner, and error policies as the
GUI. **No workflows run unless explicitly requested** — the GUI's last-used workflow selection
never applies to a headless export.

### Listing workflows

```bash
rapidraw --list-workflows
```

(The flag wins wherever it appears, so `rapidraw export ... --list-workflows` lists too.) It
prints the scanned directories, one row per workflow with **ID, phase, runtime availability,
and source**, then any unselectable entries with their reasons, scan diagnostics, and a count —
for example:

```
Bundled workflows directory: /app/resources/workflows
User workflows directory: /home/photographer/.rapidraw/workflows

ID                        PHASE      RUNTIME         SOURCE
example-post-batch-receipt postBatch node v22.11.0   bundled
example-post-image         postImage python 3.13.0   bundled
example-receipt            postBatch  python 3.13.0  bundled
my-backup                  postBatch  python unavailable user

Unselectable (invalid metadata; fix the sidecar to use them):
- broken: order must be 0..=1000

4 workflow(s) listed.
```

Runtime availability comes from the same probes as the GUI: `py -3` then `python` on Windows,
`python3` then `python` elsewhere, `node` for JavaScript; each probe is bounded to two seconds.
Listing never executes workflow code and always exits `0`.

### Selecting workflows

```bash
rapidraw export /path/to/photos --output /path/to/output_dir --workflow example-receipt
rapidraw export /path/to/photos --output /path/to/out --workflow beta --workflow alpha
```

`--workflow <id>` is repeatable; every occurrence selects one workflow and the command-line
order defines execution order within each phase (`postImage` workflows run per image in that
order after each output is fully written, `postBatch` workflows run once each after the whole
batch settles). A `--workflow` flag without a value is ignored, like every other export flag.

Requested ids are validated **before any image is exported**. An unknown id, an entry with
invalid metadata, a workflow whose runtime is unavailable, or the same id twice prints
`Headless export failed: <reason>` to stderr and exits `1` before a single image is rendered
(the output directory itself may already have been created).

### Exit codes and terminal output

| Exit code | Meaning                                                                                                                                                            |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `0`       | Export succeeded. Script warnings and `onError: warn` degradations do **not** fail the export.                                                                     |
| `1`       | Missing source, no images found, unreadable adjustments JSON, invalid/unknown/unavailable workflow id, image export error, or an `onError: fail` workflow failure. |

Selected workflows are echoed at start (`Running 2 export workflow(s) in order: beta, alpha`),
and after the batch settles every workflow run is reported:

```
Workflow 'example-receipt' succeeded for DSC_0001.NEF: wrote receipt for 1 exported of 1 selected items
Workflow 'uploader' degraded: archive offline
Workflow runs: 0 succeeded, 0 warned, 1 degraded, 0 cancelled.
```

`degraded` marks runs that failed or timed out but were tolerated by `onError: warn`; with
`onError: fail` the same failure instead fails the affected image or the whole batch and the
export exits `1`.

### Timeout and cancellation limitations

Each invocation is bounded by the workflow's `timeoutSeconds` (default 60, range 1–600 via
sidecar); a timed-out workflow is terminated tree-wide and recorded as `timedOut`, then
interpreted through its error policy. There is no headless cancellation command in this
version: interrupting the process (e.g. Ctrl+C) ends RapidRAW, and workflow subprocesses die
with it on Windows (kill-on-close job objects). On other platforms a force-killed host may
leave grandchild processes behind — the per-workflow timeout bounds them, and the tree-wide
termination paths are covered by the `workflow_security` test pass on both platform families
(see _Execution hardening_ below).

### Security

The trust model is unchanged from the GUI: workflows are **trusted local code** with your full
user permissions, discovered only from the two directories above, launched directly with an
argument array (never through a shell), with a minimal environment (`PATH`, `TEMP`/`TMP`,
`HOME`/`USERPROFILE`, plus `SYSTEMROOT`/`COMSPEC` on Windows) so secrets are not forwarded.
See [the protocol decision](decisions/export-workflow-protocol.md) for the full threat model.

## Execution hardening

The Rust runner enforces these boundaries on every desktop platform (GUI and headless), and
`cargo test --manifest-path src-tauri/Cargo.toml workflow_security` exercises each of them on
Windows and Unix CI:

- **Canonical path containment.** Discovery records each script's canonical path and rejects
  entries that resolve outside the scanned root — including sibling roots that merely share a
  name prefix. Reported script paths are always canonical.
- **Symlink/reparse-point rejection, twice.** Symlinked scripts and metadata sidecars are rejected
  during discovery, and every invocation revalidates the script right before spawn: a file that
  was deleted, replaced by a symlink, or now resolves elsewhere (or outside the roots) fails with
  a typed `ScriptIdentity` error and no subprocess is launched.
- **Interpreter pinning between probe and spawn.** Interpreters are probed and later spawned with
  argument arrays; at spawn the bare name (`py`/`python`/`python3`/`node`) is resolved through
  `PATH` (with `PATHEXT` on Windows), must canonicalize to a regular file, and that exact file is
  what launches. An interpreter that vanished or resolves to a non-file fails the spawn instead of
  silently picking a different program.
- **No environment leakage.** Children receive only the allowlisted variables above; a secret set
  in the parent environment never reaches a workflow, so it can never come back through output,
  warnings, or stderr excerpts.
- **Path quoting.** Script paths are passed as single argv entries — hostile file names with
  spaces, ampersands, dollar signs, or Unicode never pass through a shell.
- **Tree-wide cleanup.** A timed-out, cancelled, or output-exhausted run kills the whole process
  tree (kill-on-close job objects on Windows, dedicated process groups on Unix), so no child or
  known descendant survives.
- **Bounded output.** stdout, stderr, warnings, and the serialized request each have byte caps;
  exceeding one terminates the run with a typed error.
- **Fail-closed metadata.** Duplicate ids shadow deterministically with a diagnostic, hostile or
  malformed sidecars make the entry unselectable, and the engine's identity roots fail closed.

What this deliberately does **not** claim: workflows are not sandboxed. In-place content
replacement of a still-valid script path, and any attacker who can already write to `PATH` or the
workflow directories, are inside the trusted-local-code threat model. Workflow subprocesses also
never run through frontend permissions — the Tauri capability set grants the webview only
`open` for `http(s)`/`tel`/`mailto` links, and `shell:deny-execute`/`deny-spawn`/`deny-kill` are
explicitly denied (see `src-tauri/capabilities/default.json`).

## Complete examples (zero dependencies, both phases)

Save either file into `~/.rapidraw/workflows` (e.g. as `my_summary.py` / `my_summary.js`),
find its id with `rapidraw --list-workflows`, then run
`rapidraw export ... --workflow my-summary`. Each example handles both phases, uses only the
Python standard library or Node core modules, writes one summary artifact into the workspace
temp directory, and reports it.

**Python (`my_summary.py`):**

```python
#!/usr/bin/env python3
"""Minimal RapidRAW export workflow handling both phases (stdlib only)."""
import json
import os
import sys


def main():
    request = json.loads(sys.stdin.read())
    if request.get("protocolVersion") != 1:
        print(json.dumps({"ok": False, "message": "unsupported protocolVersion"}))
        return 0
    workspace = request["workspaceTempDirectory"]
    if request["phase"] == "postImage":
        summary = {
            "phase": "postImage",
            "source": request["sourcePath"],
            "exported": request["exportedPath"],
            "artifacts": [a["path"] for a in request["artifacts"]],
            "index": request["index"],
        }
    else:
        summary = {
            "phase": "postBatch",
            "exported": sum(1 for i in request["exportedItems"] if i.get("exportedPath")),
            "failed": sum(1 for i in request["exportedItems"] if i.get("error")),
        }
    name = "summary-{}.json".format(request["workflowId"])
    with open(os.path.join(workspace, name), "w", encoding="utf-8") as handle:
        json.dump(summary, handle, indent=2)
    print(json.dumps({"ok": True, "producedArtifactPaths": [name]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

**Node (`my_summary.js`):**

```js
#!/usr/bin/env node
// Minimal RapidRAW export workflow handling both phases (Node core only).
const fs = require('fs');
const path = require('path');

function readStdin() {
  return new Promise((resolve, reject) => {
    let data = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', (chunk) => (data += chunk));
    process.stdin.on('end', () => resolve(data));
    process.stdin.on('error', reject);
  });
}

async function main() {
  const request = JSON.parse(await readStdin());
  if (request.protocolVersion !== 1) {
    console.log(JSON.stringify({ ok: false, message: 'unsupported protocolVersion' }));
    return;
  }
  const workspace = request.workspaceTempDirectory;
  let summary;
  if (request.phase === 'postImage') {
    summary = {
      phase: 'postImage',
      source: request.sourcePath,
      exported: request.exportedPath,
      index: request.index,
    };
  } else {
    summary = {
      phase: 'postBatch',
      exported: request.exportedItems.filter((item) => item.exportedPath).length,
      failed: request.exportedItems.filter((item) => item.error).length,
    };
  }
  const name = `summary-${request.workflowId}.json`;
  fs.writeFileSync(path.join(workspace, name), JSON.stringify(summary, null, 2));
  console.log(JSON.stringify({ ok: true, producedArtifactPaths: [name] }));
}

main().catch((error) => {
  console.log(JSON.stringify({ ok: false, message: String(error) }));
});
```
