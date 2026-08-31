# Feature architecture

Canonical map of the three workflow features for implementation work: modules, IPC
surface, contracts, settings, tests, and safety invariants. User-facing guides:
[capture-session grouping](capture-time-grouping.md), [import](import.md),
[export workflows](export-workflows.md). Normative decisions live in
[docs/decisions](decisions).

Rust backend lives in `src-tauri/src/` (edition 2024), React frontend in `src/`. All
cross-boundary payloads are typed with serde `camelCase` on the Rust side and mirrored in
frontend contract tests (`src/test/workflowContracts.test.ts`) — never `any`.

## Preview-first import

| Concern                                        | Location                                                                                                              |
| ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Catalog, pattern engine, scan, plan, execution | `src-tauri/src/import_processing.rs`                                                                                  |
| Metadata extraction (shared)                   | `src-tauri/src/exif_processing.rs`                                                                                    |
| Settings/presets migration                     | `src-tauri/src/app_settings.rs` (`importPresets`, safe defaults)                                                      |
| UI workspace                                   | `src/components/modals/ImportSettingsModal.tsx`, `src/components/import/` (pattern builder, presets, progress/result) |
| State/hooks                                    | `src/hooks/useImportSettings.ts`, `src/store/useProcessStore.ts`, `src/hooks/useTauriListeners.ts`                    |

Commands: `create_import_plan`, `get_import_preview_page`, `execute_import_plan`,
`cancel_import`, `import_android_content_files` (Android is copy-only via content URIs).

Event: `import-progress` — `ImportProgressEvent { planId?, phase, current, total,
bytesCompleted?, bytesTotal?, sourcePath? }` with phases `scanning → planning → copying →
verifying → moving → complete | cancelled`.

Contracts: `ImportOperation` (copy/move), `CollisionPolicy` (skip/renameWithSuffix/error),
`MissingTokenPolicy` (empty/fallback/error), `ImportPattern` (versioned typed token
sequences; legacy `{TOKEN}` strings parse into them), `ImportPlanRequest` → immutable
plan id = hash of request + source stamps (any change invalidates; execution re-verifies).

Safety invariants:

- Preview is side-effect free; the frontend never supplies trusted per-row destination
  paths — execution replays the stored Rust plan by plan id.
- Destination segments are sanitized per OS (no absolute/traversal/`..`, Windows reserved
  names, illegal characters, trailing dots/spaces, length caps); containment re-checked
  immediately before each write; never silent overwrite.
- Writes go to a temp file in the destination directory, flushed, byte-count and SHA-256
  verified, then atomically renamed; `.rrdata`/`.rrexif`/`.xmp` sidecars follow.
- Move trashes a source group only after every destination in the group is verified;
  trash failure retains the source (permanent deletion fallback does not exist).
- Cancellation is cooperative and stops before the next destructive boundary; failures
  are isolated per source group; the structured result stays inspectable.

## Capture-session grouping

| Concern               | Location                                                                                                                                                                |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pure clustering       | `src/utils/captureTimeGrouping.ts` (parse + adjacent-gap sessionization)                                                                                                |
| Derived library model | `src/hooks/useSortedLibrary.ts` (filter/search → RAW/JPEG collapse → sessions)                                                                                          |
| Settings              | `src-tauri/src/app_settings.rs` + `src/store/useSettingsStore.ts` (`captureTimeGroupingEnabled`, `captureTimeGroupingMinutes`; defaults off / 15)                       |
| UI                    | `src/components/ui/AppProperties.tsx` (controls), `src/components/panel/library/LibraryHeader.tsx` (view options), `LibraryGrid.tsx` / `LibraryItems.tsx` (header rows) |

Pure frontend feature: no new IPC. Effective capture instant = EXIF `DateTimeOriginal`
(+ offset/subsec) → `CreateDate` → file modified time (counted and flagged as fallback).
A new session starts only when the gap to the immediately previous image exceeds the
threshold; folders never merge; grouping forces chronological order and restores the saved
sort when disabled. Headers are non-selectable rows excluded from every image array
(navigation, selection, export, culling).

## Export workflows

| Concern                                       | Location                                                                                                                                |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Registry, discovery, protocol, runner, engine | `src-tauri/src/export_workflows.rs`                                                                                                     |
| Export pipeline integration                   | `src-tauri/src/export_processing.rs` (`export_images_impl`, phases, events)                                                             |
| Headless CLI                                  | `src-tauri/src/launch_request.rs` (`--workflow`, `--list-workflows`)                                                                    |
| Bundled examples                              | `src-tauri/resources/workflows/` (packaged; Flatpak path in `packaging/io.github.CyberTimon.RapidRAW.yml`)                              |
| UI                                            | `src/components/export/` (multiselect, consent, result), `src/components/panel/right/ExportPanel.tsx`, `src/hooks/useExportSettings.ts` |
| Settings                                      | `src-tauri/src/app_settings.rs` (workflow ids + order in export presets/last-used)                                                      |

Commands: `discover_export_workflows`, `refresh_export_workflows` (plus `export_images` /
`cancel_export` / `estimate_export_sizes` extended with workflow ids).

Events: `workflow-progress` — `WorkflowProgressEvent { runId, phase, workflowId?,
sourcePath?, ... }` distinguishing image rendering/writing from workflow execution;
terminal state via `export-result` (`ExportResultDetail` with per-item and per-workflow
outcomes), `export-cancelled` after child cleanup.

Contracts: protocol v1 request/response per
[the decision](decisions/export-workflow-protocol.md); `postImage` runs after one output
is fully written, `postBatch` once after all image workers settle; `onError` warn/fail.

Safety invariants:

- Workflows are trusted local code, never sandboxed; discovery only scans the bundled
  resource directory and `~/.rapidraw/workflows` (top level) and never executes script code.
- Spawn uses an argument array, never a shell; children get only the allowlisted
  environment; script identity is revalidated (regular file, canonical path, containment)
  before every spawn and interpreters are pinned between probe and spawn.
- Nothing runs unless explicitly selected (GUI) or passed via `--workflow` (headless);
  first GUI use requires recorded consent; bundled examples are never auto-selected.
- Every invocation is bounded: timeout (default 60 s), stdout 256 KiB, stderr 64 KiB,
  request 1 MiB, 100 warnings; cancel/timeout/exhaustion kill the whole process tree.

## Test foundations

- Frontend: vitest + Testing Library (`src/test/` setup). Contract tests:
  `src/test/workflowContracts.test.ts`, `src/test/foundation.test.tsx`; feature suites
  under `src/components/{import,export}/`, `src/components/panel/library/`,
  `src/hooks/*.test.*`, `src/store/useProcessStore.test.ts`.
- Rust: unit tests in-module (`mod tests`), integration in `src-tauri/tests/`
  (`workflow_runner.rs`, `workflow_security.rs`, `export_workflow_integration.rs`,
  `headless_workflow_cli.rs`) with synthetic fixtures in `src-tauri/tests/fixtures/`.
- Regression matrix and platform evidence: [docs/testing/workflow-features-matrix.md](testing/workflow-features-matrix.md).
- Quality gates: `npm test` / `typecheck` / `i18n:check` / `build` and `cargo fmt` /
  `clippy --all-targets` / `test` (see `AGENTS.md` for the Windows toolchain PATH hazard;
  `--all-features` clippy is Linux/CI-only).
