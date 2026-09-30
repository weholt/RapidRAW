# AGENTS.md

Agent instructions for RapidRAW (Tauri 2 + React RAW editor). Rust toolchain section
seeded by rapidraw-3cb; canonical command set, CI entry points, and the RED-GREEN
Pebbles workflow added by rapidraw-e3d; Lap extraction coordination added by
rapidraw-dfa.

## Project purpose

RapidRAW is a GPU-accelerated, non-destructive RAW photo editor. Rust backend in
`src-tauri/` (edition 2024, MSRV 1.98), TypeScript/React frontend in `src/`.

## Canonical quality gates

Run these before declaring any work done. Frontend commands run from the repo root
and match the `scripts` in `package.json`:

```powershell
npm test              # vitest run
npm run typecheck     # tsc --noEmit
npm run lint          # eslint .
npm run format:check  # prettier --check .
npm run i18n:check    # i18next extraction sync + runtime key check
```

Fixers when needed: `npm run lint:fix`, `npm run format`, `npm run i18n:extract`.

Rust gates (only after the PATH prepend from the toolchain section below):

```powershell
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets
```

Do not use `--all-features` locally on Windows: the `tethering` feature gates
libgphoto2 and only builds on Linux (see CI section).

## CI entry points

- `.github/workflows/lint.yml` — **Lint**, on PRs and pushes to `main`. The local
  mirror of its steps:
  - `frontend-lint` job: `npm ci`, then `npm test`, `npm run typecheck`, followed by
    `npm run format:check`, `npm run lint`, `npm run i18n:check` (those last three are
    `continue-on-error` today, so treat local runs as the real gate).
  - `fmt` job: `cargo fmt -p RapidRAW -- --check` from `src-tauri/`.
  - `test` job: `cargo test --manifest-path src-tauri/Cargo.toml` on Ubuntu with the
    Tauri system deps (webkit2gtk, appindicator, librsvg).
  - `clippy` job: `cargo clippy --all-targets --all-features -- -D warnings` on Ubuntu
    with `libgphoto2-dev` — the only place `--all-features` runs.
- `.github/workflows/pr-ci.yml` — full cross-platform build matrix on every PR.
- `.github/workflows/ci.yml` — same matrix on pushes to `main`.
- `.github/workflows/build.yml` — reusable `workflow_call` target used by both matrices.
- `.github/workflows/release.yml` — packages and uploads app bundles on GitHub releases.

## Rust toolchain on this Windows machine — read before any cargo command

`src-tauri/rust-toolchain.toml` pins channel `1.98`, which resolves to the rustup
toolchain `1.98.0-x86_64-pc-windows-msvc`.

**Hazard:** the Machine PATH contains `C:\Program Files (x86)\Rust stable GNU 1.85\bin`
before any rustup entry. In a plain shell, `cargo`, `rustc`, `rustdoc`, `rustfmt`, and
`clippy-driver` all resolve to the standalone 1.85 i686-pc-windows-gnu toolchain.
Building with it poisons `src-tauri/target` with mixed-compiler artifacts (E0514,
dlltool/raw-dylib failures, E0658 let-chain errors).

### Canonical invocation

Prepend the rustup bin dir for the session, then use plain cargo — this makes every
tool resolve through rustup and the pinned toolchain:

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets
```

Never run `cargo fmt` or `cargo clippy` without the PATH prepend: they resolve their
driver binaries from PATH and would get the 1.85 GNU versions, which cannot parse
edition-2024 let-chains.

### Automatic guard for builds and rustdoc (no env needed)

A machine-local, gitignored `.cargo/config.toml` at the repo root pins
`build.rustc`/`build.rustdoc` to the rustup `1.98.0-x86_64-pc-windows-msvc` binaries,
so any `cargo build|test|run|doc` compiles with one compiler even when the standalone
cargo is invoked. Recreate it on a fresh clone of this machine with:

```powershell
New-Item -ItemType Directory -Force .cargo | Out-Null
@'
[build]
rustc = 'C:\Users\Thomas\.rustup\toolchains\1.98.0-x86_64-pc-windows-msvc\bin\rustc.exe'
rustdoc = 'C:\Users\Thomas\.rustup\toolchains\1.98.0-x86_64-pc-windows-msvc\bin\rustdoc.exe'
'@ | Set-Content .cargo\config.toml
```

If the config file is absent, the equivalent per-shell fallback is:

```powershell
$env:RUSTC   = "$env:USERPROFILE\.rustup\toolchains\1.98.0-x86_64-pc-windows-msvc\bin\rustc.exe"
$env:RUSTDOC = "$env:USERPROFILE\.rustup\toolchains\1.98.0-x86_64-pc-windows-msvc\bin\rustdoc.exe"
```

### Permanent fix (removes the hazard and both guards)

Remove the standalone entry from the Machine PATH with one elevated PowerShell:

```powershell
$k = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Control\Session Manager\Environment', $true)
$raw = $k.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$kind = $k.GetValueKind('Path')
$parts = $raw -split ';' | Where-Object { $_ -and $_ -ne 'C:\Program Files (x86)\Rust stable GNU 1.85\bin' }
$k.SetValue('Path', ($parts -join ';'), $kind); $k.Close()
```

Afterwards delete the repo-local `.cargo/config.toml`; the PATH prepend becomes
unnecessary (but harmless). The standalone install itself is left in place and can
still be used by absolute path if ever needed.

### Notes

- GNU-target work that needs `dlltool`/`as`: w64devkit lives at
  `C:\Users\Thomas\Tools\w64devkit`.
- `cargo clippy --all-features` is Linux-only (the `tethering` feature gates
  libgphoto2) and is enforced in `.github/workflows/lint.yml`.
- CI is unaffected by the PATH hazard: GitHub runners use rustup-managed toolchains
  selected by `rust-toolchain.toml`; the machine-local pin is gitignored and never
  leaves this machine.

## Lap extraction coordination

This checkout (`feature/lap-engine-extraction`) is the isolated extraction host
for the Lap RAW-development plan PLAN-260926, coordinated from the Lap repository
at `C:/Users/Thomas/Desktop/lap` (governing contract: `docs/raw-development/spec.md`
there; baseline gates in `docs/raw-development/baseline.md`). Rules for work here:

- Every Lap task that touches this checkout creates or reuses a linked Pebbles
  issue here (e.g. rapidraw-dfa for Lap lap-7f5.1/TASK-101) and records the
  coordinating Lap issue ID before source edits.
- `C:/Users/Thomas/Desktop/RapidRAW` is reference-only; never modify it.
- No pushes, publication, or deployment; local commits reference the issue ID.
- Shared schema/engine changes stay authoritative for both hosts; Lap consumes a
  pinned engine revision rather than an unversioned branch.

## Pebbles workflow (RED-GREEN)

`.pebbles/events.jsonl` is the durable, append-only work record. All issue state
lives there and is mutated only through the `pb` CLI — never hand-edit the log.

1. Pick the issue with `pb ready` / `pb show <id>`, then
   `pb update <id> --status in_progress` before touching code.
2. **RED** — write the failing test first, run the matching canonical gate, and
   record the failure as evidence: `pb comment <id> --body "RED: <command + observed failure>"`.
3. **GREEN** — make the test pass with the least change, rerun the gate, and
   record the passing run: `pb comment <id> --body "GREEN: <command + observed pass>"`.
4. **REFACTOR** only while the gates stay green; rerun the gates after.
5. `pb close <id>` only when the issue's acceptance criteria and verification
   steps pass, and reference the issue ID in the commit message.

Issue IDs may appear in docs (like the provenance line at the top of this file)
as process history only — never as a status report or backlog copy.

## Safety

- Never commit `.cargo/config.toml`, `src-tauri/target/`, or other machine-local state.
- Do not hand-edit `.pebbles/events.jsonl`; mutate Pebbles only through the `pb` CLI.
- Keep changes scoped to the active Pebbles issue and reference its ID in commits.
- No secrets, volatile status, or backlog copies in agent docs — process only.
