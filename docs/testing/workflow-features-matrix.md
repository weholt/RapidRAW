# Workflow features regression matrix

End-to-end matrix for the three workflow features — preview-first import, capture-session
grouping, and export workflows — covering happy paths, cancellation at every destructive
boundary, restart persistence, and platform parity. Track work with the `rapidraw-616.*`
Pebbles issues; record new failures as new issues rather than comments.

Fixtures are synthetic or repository assets only: Rust import tests synthesize JPEG/RAW,
`.rrdata`, `.rrexif`, and XMP files in temp directories; workflow tests use the committed
`src-tauri/tests/fixtures/workflows/` scripts (stdlib-only Python/Node, no credentials);
manual exports use `public/splash-light.jpg`. No private photographs or metadata are used.

## Quality gates

Local run (Windows 11, Rust 1.98 via rustup, Node 22, 2026-08-31, branch
`pebbles-harness/20260830-144016`):

| Gate                         | Result | Notes                                                                                                                                                                                                                                            |
| ---------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `npm test`                   | PASS   | 77 tests / 17 files                                                                                                                                                                                                                              |
| `npm run typecheck`          | PASS   |                                                                                                                                                                                                                                                  |
| `npm run build`              | PASS   | 2557 modules                                                                                                                                                                                                                                     |
| `npm run i18n:check`         | PASS   | 1395 plural resolutions, 13 locales, extraction clean                                                                                                                                                                                            |
| `npm run format:check`       | DRIFT  | 207 files locally, but 187 are a CRLF artifact: `core.autocrlf=true` checks out CRLF while Prettier defaults to LF. Line-ending-agnostic recheck (`--end-of-line auto`) shows 20 drifted files — all pre-existing; every new/feature file passes |
| `npm run lint`               | DRIFT  | Pre-existing repo-wide failures (919 problems); all new feature files lint clean; CI step is `continue-on-error`                                                                                                                                 |
| `cargo fmt -- --check`       | PASS   |                                                                                                                                                                                                                                                  |
| `cargo clippy --all-targets` | PASS   | `--all-features` is Linux-only (tethering/libgphoto2); enforced in CI `lint.yml`                                                                                                                                                                 |
| `cargo test`                 | PASS   | 57 unit + 73 integration tests                                                                                                                                                                                                                   |

CI (`.github/workflows`): `lint.yml` runs frontend tests + typecheck (blocking), cargo
fmt/test and clippy `--all-features -D warnings` on Ubuntu; `pr-ci.yml`/`ci.yml` build
Windows x64/ARM, macOS 14/15 (arm + Intel, with and without tethering), Ubuntu 22.04/24.04
(x64 + ARM), and Android. Format/lint/i18n steps are `continue-on-error` upstream.

Known open issue: `rapidraw-0f5` — rare race in the import scan cancellation test on the
global scan-cancel flag. Not observed in this pass (Rust suite passed repeatedly); tracked
separately and does not block this matrix.

## Automated coverage

Cancellation and safety boundaries are covered by named test passes; each row maps a matrix
cell to the tests that pin it.

| Matrix cell                                                                                                                                                                  | Coverage                                                                                                                                                                                                                             |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Import metadata precedence (`.rrdata` > `.rrexif` > XMP > embedded) without writes                                                                                           | `metadata_catalog_sidecars_override_xmp_and_embedded_without_writes` (Rust unit)                                                                                                                                                     |
| Pattern engine: traversal, absolute paths, reserved names, Unicode, length, duplicates                                                                                       | `pattern_engine_rejects_traversal_absolute_and_reserved_names`, `pattern_engine_missing_policy_and_legacy_compatibility`, `pattern_engine_preserves_legacy_token_names` (Rust unit)                                                  |
| Import scan: recursion, unsupported files, plan invalidation, duplicates, paging, side-effect-free preview                                                                   | `import_scan_recursion_unsupported_and_plan_invalidation`, `import_plan_is_paged_side_effect_free_and_detects_duplicates` (Rust unit)                                                                                                |
| Import execution: verified copy, sidecars, stale-plan rejection, move/trash safety, per-group failure isolation                                                              | `import_execution_copy_is_verified_and_stale_plan_is_rejected`, `import_execution_end_to_end_copy_sidecars_and_safety_paths` (Rust unit)                                                                                             |
| Import UI: debounced authoritative preview, blocking errors disable Import, plan identity, presets, progress/result                                                          | `ImportSettingsModal.test.tsx`, `ImportPatternBuilder.test.tsx`, `ImportPresetsList.test.tsx`, `ImportProgress.test.tsx` (frontend)                                                                                                  |
| Capture grouping: EXIF parse, offsets, subseconds, fallback chain, adjacent-gap equality, chaining, folder separation, ties, stable IDs                                      | `captureTimeGrouping.test.ts` (6 tests incl. 10,000-image budget)                                                                                                                                                                    |
| Derived library: filter/search before sessionization, RAW/JPEG collapse before headers, no cross-folder merge, late EXIF regroup, sort preservation                          | `useSortedLibrary.test.ts` (5 tests)                                                                                                                                                                                                 |
| Session header rows: fixed-height, non-selectable, accessible                                                                                                                | `LibraryItems.test.tsx`; control persistence in `LibraryHeader.test.tsx`                                                                                                                                                             |
| Workflow discovery: defaults, sidecar overrides, duplicate ids, symlinks, containment, unavailable runtimes, refresh diff                                                    | `workflow_discovery_*` (Rust unit, 10 tests), `ExportWorkflowSelect.test.tsx` (14 tests)                                                                                                                                             |
| Workflow runner: protocol fixtures, literal argv, stdout/stderr caps, timeout, cancellation of sleeping Python/Node children, process-tree kill, minimal environment         | `workflow_runner_*` (Rust unit + `tests/workflow_runner.rs`, 24 tests)                                                                                                                                                               |
| Workflow/export integration: phase ordering, artifact chaining, warn/fail policies, cancellation skips unstarted, postBatch once after settle, empty selection = no overhead | `tests/export_workflow_integration.rs` (29 tests)                                                                                                                                                                                    |
| Workflow security: hostile names/metadata, spawn-time identity revalidation, interpreter pinning, env leak, output exhaustion                                                | `tests/workflow_security.rs` (17 tests)                                                                                                                                                                                              |
| Headless CLI: repeated `--workflow` order, listing, unknown/unavailable/duplicate ids fail before export, exit codes                                                         | `launch_parser_*` (Rust unit), `tests/headless_workflow_cli.rs` (10 tests)                                                                                                                                                           |
| Export UI: discovery on open, multiselect/ordering, consent gate + persistence, Android hidden, phase-labeled progress, terminal result detail                               | `ExportPanel.test.tsx` (9 tests), `ExportWorkflowConsent.test.tsx` (6 tests), `ExportResult.test.tsx` (5 tests), `useExportSettings.test.tsx` (4 tests), `useProcessStore.test.ts` (3 tests), `useTauriListeners.test.tsx` (5 tests) |
| Cross-boundary contracts (import plan, workflow request/progress, result detail)                                                                                             | `src/test/workflowContracts.test.ts` (4 tests)                                                                                                                                                                                       |

## Manual matrix (per platform)

Repeat on Windows, macOS, and Linux. Automated rows above run identically on all platforms
through CI; the rows below need a desktop session. Record PASS/FAIL with a screenshot or
log line on the parent Pebbles issue.

| #   | Scenario                                               | Steps                                                                                                                 | Expected                                                                                                                                     |
| --- | ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| M1  | Import happy path (copy)                               | Import a mixed folder (JPEG, RAW, RAW+JPEG, `.xmp`) with pattern `{YYYY}/{MM}/{original_filename}`; preview, then run | Destinations match preview exactly; sources untouched; sidecars follow renamed images; summary shows verified counts; library refreshes once |
| M2  | Import collision handling                              | Import twice into the same destination (default rename policy, then skip)                                             | No silent overwrite; renamed/skipped rows reported in result                                                                                 |
| M3  | Cancel during scan / plan / copy / before source trash | Cancel at each phase of a move import                                                                                 | Sources and destinations never both absent; partial state recoverable; result stays inspectable                                              |
| M4  | Import restart persistence                             | Set patterns + presets, restart app                                                                                   | Presets and last-used locations persist; defaults remain copy + safe collision                                                               |
| M5  | Capture grouping browse                                | Enable Capture Sessions, vary threshold 1–120, toggle recursive folders                                               | Headers split on gaps > threshold; equality stays; folders never merge; fallback indicator when EXIF missing                                 |
| M6  | Grouping + variant selection export                    | Collapse RAW+JPEG, select grouped items across sessions, export                                                       | Exports exactly the selected logical paths per existing variant semantics; headers never enter selection                                     |
| M7  | Export with both workflow phases                       | Select one `postImage` and one `postBatch` workflow (order set in UI), export                                         | First-run consent prompt once; per-image receipts after each output; batch summary at end; order preserved                                   |
| M8  | Cancel during workflow phases                          | Cancel during post-image and post-batch runs                                                                          | State reaches Cancelled after child cleanup; no orphan interpreter processes remain                                                          |
| M9  | No workflow by default                                 | Export with no workflows selected; headless export without `--workflow`                                               | Zero subprocess overhead; headless runs nothing                                                                                              |
| M10 | Workflow trust + refresh                               | Add/remove scripts in `~/.rapidraw/workflows` while panel open; refresh                                               | List updates without restart; unavailable runtimes disabled with reason; consent persists per installation                                   |
| M11 | Restart + large library                                | Restart on a large library with grouping enabled                                                                      | Settings stable; grouping of 10k images within interactive budget (automated: 17 ms < 1000 ms, no I/O)                                       |

### Evidence (Windows, 2026-08-31)

- `rapidraw --list-workflows` listed the three bundled examples with runtime versions
  (`py Python 3.13.1`, `node v22.19.0`), source, and a `workflow.discovery.root.missing`
  diagnostic for the absent user root — matches `docs/export-workflows.md` output format.
- Headless export `--workflow example-receipt`: `Workflow 'example-receipt' succeeded:
wrote receipt for 1 exported of 1 selected items`, `Workflow runs: 1 succeeded`,
  exit `0`, output `splash-light_edited.jpeg` written.
- Headless export `--workflow does-not-exist`: `Headless export failed: workflow
'does-not-exist' is not in the workflow registry`, exit `1`, no image rendered.
- Full automated suites and gates per the table above.
- Re-verified 2026-08-31 (second pass): all gates green with identical results
  (`npm test` 77/77, cargo 57 unit + 73 integration, clippy clean). Headless rows
  reproduced: listing exit `0`; `--workflow example-receipt` exit `0` with a 2048×2048
  GPU render at 148.8 ms and a 1.8 MB output plus receipt; unknown id exit `1` with no
  output file; export **without** `--workflow` printed no workflow lines at all —
  zero subprocess overhead (M9 headless half).

macOS/Linux: no runs exist yet — the four proposal branches (see
`docs/testing/upstream-proposals.md`) are local-only pending push authorization, so
`lint.yml` (Ubuntu: frontend tests, cargo fmt/test, clippy `--all-features`) and the
`pr-ci.yml`/`ci.yml` build matrix will produce the automated-platform evidence as soon
as they are pushed. Desktop-session rows M1–M11 on macOS/Linux remain manual after
that; tracked as CI/manual evidence tasks on `rapidraw-616.1`.

## Performance references

- Capture grouping: 10,000-image synthetic benchmark inside the interactive budget
  (17 ms measured, 1000 ms ceiling, no filesystem calls) — `captureTimeGrouping.test.ts`.
- Headless single-image export with one postBatch workflow: end-to-end well under the
  60 s default workflow timeout (GPU processing 150 ms, workflow run sub-second in the
  evidence pass above).
- Import preview is side-effect free and paged; execution verifies byte counts plus
  SHA-256 per file (see `docs/import.md`).
