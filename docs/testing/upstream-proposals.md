# Upstream proposal series

Four stacked, independently reviewable proposals carry the three workflow features.
Each branch's stated test plan was executed at its boundary (Windows 11, Rust 1.98,
Node 22, 2026-08-31). Nothing is pushed — opening PRs requires explicit user
authorization. Never force-push; the series rebases onto `main` as a stack.

| #   | Branch (local)                         | Base          | Size     | Pebbles epic               |
| --- | -------------------------------------- | ------------- | -------- | -------------------------- |
| 1   | `proposal/1-test-typecheck-foundation` | `origin/main` | 44 files | rapidraw-fe3               |
| 2   | `proposal/2-capture-session-grouping`  | proposal/1    | 29 files | rapidraw-90e               |
| 3   | `proposal/3-safe-metadata-import`      | proposal/2    | 37 files | rapidraw-e5b               |
| 4   | `proposal/4-export-workflows`          | proposal/3    | 62 files | rapidraw-060, rapidraw-616 |

`proposal/4` is content-identical to the integrated state for every project file
(`git diff proposal/4 pebbles-harness/20260830-144016` shows only machine/harness
files: `AGENTS.md`, `.agents/`, `.pebbles/`, `.gitignore`, the plan file).

## 1 — Test and typecheck foundation (rapidraw-fe3.2, rapidraw-fe3.3)

**User value.** Makes `npm run typecheck` pass for the first time (main fails with
dozens of errors) and adds a vitest/jsdom foundation (`npm test`, CI steps in
`lint.yml` including a `cargo test` job), enabling every later change to land tested.

**Compatibility.** No behavior change except strictly-typed annotations and i18n
catalog normalization (`_one`/`_other`/`_many` plurals, canonical ordering); no key
values change.

**Security.** None affected; one clippy let-chain cleanup in `exif_processing.rs`.

**Verification run at this boundary.** `npm run typecheck` (failing on main → passing),
`npm test` (2 foundation tests), `npm run build`, `npm run i18n:runtime-check`
(1147 resolutions), `cargo fmt -- --check`, `cargo clippy --all-targets`,
`cargo test` — all green.

## 2 — Capture-session grouping (rapidraw-90e)

**User value.** Optional library view sections that group visible images into capture
sessions using EXIF-capture-time adjacent gaps (1–120 min), with accessible,
non-selectable headers in grid/list, fallback indicator for missing EXIF, and
recursive-folder safety.

**Compatibility defaults.** Disabled by default; 15-minute threshold; old settings
deserialize with these defaults (serde test); disabling restores the user's prior sort;
RAW/JPEG variant semantics untouched.

**Security/performance.** Pure frontend; no new IPC. 10,000-image grouping measured
17 ms against a 1000 ms budget with zero filesystem calls (in-test benchmark).

**Verification run at this boundary.** typecheck, `npm test` (15 tests incl. 6
capture-grouping tests), `cargo test app_settings` (serde compatibility), all green.

## 3 — Safe metadata import (rapidraw-e5b)

**User value.** Replaces the old template-string import with a preview-first planner:
typed folder/filename token patterns (EXIF/XMP-backed), explicit collision and
missing-token policies, verified copy/move with sidecars, structured results,
cancellable phases.

**Deletion safety (explicitly changed).** Move now trashes a source group only after
every destination in the group is written and SHA-256/byte verified; a trash failure
retains the source and reports an error — the legacy permanent-delete fallback is
removed, and Android move/delete is refused rather than faked. Path resolution
sanitizes every segment (traversal, absolute, Windows reserved names, lengths),
re-checks containment before each write, and never silently overwrites.

**Compatibility defaults.** Operation copy, collision rename-with-suffix, associated
sidecars included; presets default safely on old settings; legacy `{TOKEN}` templates
keep working; the frontend never supplies trusted per-row destination paths.

**Verification run at this boundary.** typecheck, `npm test` (26 tests incl. import
UI/pattern/preset/progress suites), full `cargo test` (11 unit tests: catalog
precedence, pattern engine safety, scan/plan paging, verified execution, stale-plan
rejection), `cargo clippy --all-targets` — all green.

## 4 — Export workflows (rapidraw-060, integration rapidraw-616)

**User value.** Trusted local `.py`/`.js` automation scripts discovered from the
bundled resources and `~/.rapidraw/workflows`, selectable and orderable per export
(postImage/postBatch phases), with progress, bounded logs, cancellation, result
detail, first-run consent, headless `--workflow`/`--list-workflows`, and shipped
examples.

**Trusted code execution (explicit).** Workflows run with full user permissions and
are **not sandboxed**; the docs and consent dialog say so. Structural mitigations:
only two scanned directories, discovery never executes code, nothing runs unless
explicitly selected (headless needs `--workflow`), argument-array spawn without a
shell, minimal allowlisted environment, spawn-time script-identity and interpreter
revalidation, tree-wide kill on cancel/timeout/exhaustion, byte caps everywhere, and
frontend shell permissions explicitly denied (`shell:deny-execute/-spawn/-kill`).

**Compatibility defaults.** Zero workflows selected by default; no-workflow exports
follow the existing path with no subprocess overhead; export presets round-trip
workflow ids; Android hides the section; existing CLI flags unchanged.

**Verification run at this boundary.** typecheck, `npm test` (77 tests), full
`cargo test` (130 tests: discovery, runner, integration, headless, security passes),
`cargo clippy --all-targets`, `cargo fmt -- --check`, `npm run i18n:check`,
`npm run build`, plus manual headless evidence (listing, `--workflow` success and
unknown-id failure with correct exit codes) recorded in
[the regression matrix](testing/workflow-features-matrix.md).

## Screenshots

UI screenshots (library session headers, import workspace, export workflow multiselect
and consent dialog) require an interactive desktop session and are pending; tracked
with the manual matrix rows on `rapidraw-616.1`.
