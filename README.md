# CursorPeek

CursorPeek is a lightweight native Windows utility that previews supported local image and text
files when you hover over them in File Explorer.

It stays in the notification area, keeps Explorer focused, works offline, and does not require an
account, cloud service, background server, or external runtime.

> CursorPeek is preparing its first release. Until packaged downloads are published, build it from
> source or use a test build supplied for qualification.

## What it previews

- Images: JPEG, PNG, GIF, WebP, BMP/DIB, ICO, and TIFF
- Text: common plain-text, source-code, script, markup, data, and configuration files
- Extensionless project files such as `README`, `LICENSE`, `Makefile`, and `Dockerfile`

CursorPeek intentionally does not preview folders, network paths, virtual Shell items, or cloud
placeholders that require download. Text is displayed as inert plain text; markup and scripts are
never executed.

See the [user guide](docs/USER_GUIDE.md) for the complete format list, portable mode, tray
settings, limits, diagnostics, and troubleshooting. See [PRIVACY.md](PRIVACY.md) for the local-only
data policy and containment boundary.

## Requirements

- Windows 10 22H2 x64 or Windows 11 x64
- File Explorer

Packaged builds are self-contained and do not require Rust or the Visual C++ Redistributable.

## Build from source

Install the Rust version pinned in `rust-toolchain.toml` and Visual Studio Build Tools with the
Desktop development with C++ workload. Then run:

```powershell
cargo build --locked --release
.\target\release\CursorPeek.exe
```

The release executable is written to `target\release\CursorPeek.exe`.

## Development checks

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo test --locked --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked --release
.\tools\Test-WindowsQuality.ps1
```

Public diagnostic commands:

```powershell
.\target\release\CursorPeek.exe --input-diagnostics
.\target\release\CursorPeek.exe --worker-diagnostics
```

## Contributing

Bug reports, focused feature proposals, documentation improvements, and code contributions are
welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Report security
issues privately as described in [SECURITY.md](SECURITY.md).

## License

CursorPeek is available under your choice of the
[Apache License 2.0](LICENSE-APACHE) or the [MIT License](LICENSE-MIT).
