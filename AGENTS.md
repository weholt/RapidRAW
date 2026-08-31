# AGENTS.md

Agent instructions for RapidRAW (Tauri 2 + React RAW editor). Seeded by rapidraw-3cb
with the Rust toolchain section; rapidraw-e3d expands this file with the full canonical
command set, CI entry points, and the RED-GREEN Pebbles workflow.

## Project purpose

RapidRAW is a GPU-accelerated, non-destructive RAW photo editor. Rust backend in
`src-tauri/` (edition 2024, MSRV 1.98), TypeScript/React frontend in `src/`.

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

## Safety

- Never commit `.cargo/config.toml`, `src-tauri/target/`, or other machine-local state.
- Do not hand-edit `.pebbles/events.jsonl`; mutate Pebbles only through the `pb` CLI.
- Keep changes scoped to the active Pebbles issue and reference its ID in commits.
