# Contributing to CursorPeek

Thanks for helping improve CursorPeek. Small, focused contributions are easiest to review and keep
the utility lightweight.

## Before starting

- Use the issue forms for reproducible bugs and focused feature proposals.
- Discuss substantial UI, format, dependency, packaging, or security-boundary changes before
  implementing them.
- Do not post vulnerability details publicly. Follow [SECURITY.md](SECURITY.md).
- Keep version 0.1 changes within the scope described in the README and existing implementation.

## Development setup

CursorPeek currently builds for 64-bit Windows with the MSVC toolchain. Install:

- the Rust version pinned in `rust-toolchain.toml`;
- the `x86_64-pc-windows-msvc` target;
- Visual Studio Build Tools with the Desktop development with C++ workload.

From PowerShell:

```powershell
cargo fmt --all -- --check
cargo fmt --manifest-path fuzz/Cargo.toml --all -- --check
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- `
    -D warnings -D clippy::undocumented_unsafe_blocks
cargo build --locked --release
cargo deny --locked --workspace check
```

Changes to Windows resources or the release executable should also run:

```powershell
.\tools\Test-PerformanceBudget.ps1
.\tools\Test-RecoverySoak.ps1
.\tools\Test-WindowsQuality.ps1
.\tools\New-ReleaseSbom.ps1
```

Tests that exercise shared qualification paths must run sequentially. Real Explorer behavior,
focus, DPI, input, sleep, and packaging changes need reviewable Windows evidence in addition to
unit tests. The platform-neutral parser fuzz targets have a separate pinned setup documented in
[fuzz/README.md](fuzz/README.md).

## Design expectations

- Keep file parsing in the contained worker, never in the tray/UI process.
- Treat file bytes, Shell data, IPC frames, settings, and image dimensions as untrusted.
- Preserve fail-closed path resolution and the local-only, no-recall content policy.
- Keep UI-thread work nonblocking and preview windows non-activating.
- Use checked arithmetic before allocation or protocol sizing.
- Keep `unsafe` code within small Windows boundaries and document its safety assumptions.
- Avoid new dependencies unless their features, licenses, advisories, build behavior, and binary
  impact have been reviewed.

## Pull requests

Each pull request should explain the user-visible reason for the change, the implementation
boundary, risks, and the exact validation performed. Keep generated output and unrelated
formatting out of the diff. Update documentation and tests with the behavior they describe.

The project does not require a contributor license agreement. Unless explicitly stated otherwise,
an intentionally submitted contribution is offered under the project's
`MIT OR Apache-2.0` license terms.

All participation is subject to [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
