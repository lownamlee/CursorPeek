# CursorPeek

CursorPeek is a lightweight native Windows utility that previews supported local image and text
files when you hover over them in File Explorer.

It stays in the notification area, keeps Explorer focused, works offline, and does not require an
account, cloud service, background server, or external runtime.

> CursorPeek 0.1.0 is available as a
> [per-user installer](https://github.com/lownamlee/CursorPeek/releases/latest/download/CursorPeek-0.1.0-windows-x64-setup.exe)
> or a
> [portable ZIP](https://github.com/lownamlee/CursorPeek/releases/latest/download/CursorPeek-0.1.0-windows-x64-portable.zip).
> The packages are unsigned; review the [0.1 changelog](CHANGELOG.md), verify the
> [published checksums](https://github.com/lownamlee/CursorPeek/releases/latest/download/SHA256SUMS.txt),
> and read the [known limitations](docs/KNOWN_LIMITATIONS.md) before running them.

## What it previews

- Images: JPEG, PNG, GIF, WebP, BMP/DIB, ICO, and TIFF
- Text: common plain-text, source-code, script, markup, data, and configuration files
- Extensionless project files such as `README`, `LICENSE`, `Makefile`, and `Dockerfile`

CursorPeek intentionally does not preview folders, network paths, virtual Shell items, or cloud
placeholders that require download. Text is displayed as inert plain text; markup and scripts are
never executed.

See the [user guide](docs/USER_GUIDE.md) for the complete format list, portable mode, tray
settings, limits, diagnostics, and troubleshooting. The
[known limitations](docs/KNOWN_LIMITATIONS.md) describe the deliberate 0.1 scope. See
[PRIVACY.md](PRIVACY.md) for the local-only data policy and containment boundary.

## Requirements

- Windows 10 22H2 x64 or Windows 11 x64
- File Explorer

Packaged builds are self-contained and do not require Rust or the Visual C++ Redistributable.

## Install or run

The per-user installer requests no administrator elevation and supports Start Menu, desktop, and
startup options. For a portable copy, extract the complete ZIP and run `CursorPeek.exe`; keep
`CursorPeek.portable` beside it so settings remain in that folder.

Both packages, their checksums, the CycloneDX SBOM, and build provenance are published on the
[CursorPeek 0.1.0 release](https://github.com/lownamlee/CursorPeek/releases/tag/v0.1.0).

## Build from source

Install the Rust version pinned in `rust-toolchain.toml` and Visual Studio Build Tools with the
Desktop development with C++ workload. Then run:

```powershell
cargo build --locked --release
.\target\release\CursorPeek.exe
```

The release executable is written to `target\release\CursorPeek.exe`.

To create and validate a local portable package from a clean source tree:

```powershell
.\tools\New-PortablePackage.ps1
.\tools\Test-PortablePackage.ps1 `
    -PackagePath .\target\packages\CursorPeek-0.1.0-windows-x64-portable.zip
```

The ZIP contains the portable marker, changelog, known limitations, user and security
documentation, project licenses, exact third-party license files, release metadata, and internal
checksums. Its adjacent `.sha256` file verifies the complete archive.

To build the per-user installer from that qualified portable archive:

```powershell
$portable = Get-ChildItem .\target\packages\*-portable.zip
$nsis = .\tools\Get-Nsis.ps1
.\tools\New-InstallerPackage.ps1 `
    -PortablePackage $portable.FullName `
    -NsisCompiler $nsis
.\tools\Test-InstallerPackage.ps1 `
    -InstallerPath .\target\packages\CursorPeek-0.1.0-windows-x64-setup.exe
```

The NSIS compiler is downloaded from its official distribution and accepted only when its pinned
SHA-256 hash and version match. The installer targets the current user, requests no elevation, and
supports repair, optional shortcuts, optional startup, and settings-aware uninstall. Its payload
also includes the exact NSIS packaging license notice.

## Development checks

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo test --locked --all-features
cargo clippy --locked --all-targets --all-features -- `
    -D warnings -D clippy::undocumented_unsafe_blocks
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
