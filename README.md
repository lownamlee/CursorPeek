<p align="center">
  <picture>
    <source
      media="(prefers-color-scheme: dark)"
      srcset="./assets/readme/CursorPeek_banner_dark.png"
    />
    <source
      media="(prefers-color-scheme: light)"
      srcset="./assets/readme/CursorPeek_banner_white.png"
    />
    <img
      alt="Windows CursorPeek — quick file previews in File Explorer"
      src="./assets/readme/CursorPeek_banner_white.png"
    />
  </picture>
</p>

<p align="center">
  <strong>Native</strong> · <strong>Offline</strong> · <strong>No account</strong> ·
  <strong>No shell extension</strong>
</p>

<p align="center">
  <picture>
    <source
      media="(prefers-color-scheme: dark)"
      srcset="./assets/readme/CursorPeek_divider_dark.svg"
    />
    <source
      media="(prefers-color-scheme: light)"
      srcset="./assets/readme/CursorPeek_divider_light.svg"
    />
    <img
      alt=""
      src="./assets/readme/CursorPeek_divider_light.svg"
      width="100%"
      height="1"
    />
  </picture>
</p>

<h3 align="center">
  <a href="#-installation">Installation</a>
  <span> · </span>
  <a href="docs/USER_GUIDE.md">Documentation</a>
  <span> · </span>
  <a href="https://github.com/lownamlee/CursorPeek/releases">Release notes</a>
  <span> · </span>
  <a href="https://github.com/lownamlee/CursorPeek/issues">Issues</a>
</h3>

<p align="center">
  <a href="https://github.com/lownamlee/CursorPeek/releases/latest">
    <img
      alt="Latest release"
      src="https://img.shields.io/github/v/release/lownamlee/CursorPeek?display_name=tag&sort=semver"
    />
  </a>
  <a href="https://github.com/lownamlee/CursorPeek/actions/workflows/windows-quality.yml">
    <img
      alt="Windows quality"
      src="https://github.com/lownamlee/CursorPeek/actions/workflows/windows-quality.yml/badge.svg?branch=main"
    />
  </a>
  <a href="#-license">
    <img
      alt="License: MIT OR Apache-2.0"
      src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-2563eb"
    />
  </a>
</p>

## 📦 Installation

Requires Windows 10 22H2 x64 or supported Windows 11 x64 with Windows File Explorer.

| Package | Download | Use |
| --- | --- | --- |
| **Per-user installer** | **[Download v0.2.1](https://github.com/lownamlee/CursorPeek/releases/latest/download/CursorPeek-0.2.1-windows-x64-setup.exe)** | Recommended; installs without administrator elevation |
| **Portable ZIP** | **[Download v0.2.1](https://github.com/lownamlee/CursorPeek/releases/latest/download/CursorPeek-0.2.1-windows-x64-portable.zip)** | Extract the archive and run `CursorPeek.exe` |

> [!NOTE]
> Packages are not code-signed yet. Windows may show an unknown-publisher warning. Check downloads
> against the published [SHA256 hashes](https://github.com/lownamlee/CursorPeek/releases/latest/download/SHA256SUMS.txt).

See the [user guide](docs/USER_GUIDE.md#installed-and-portable-settings) for startup, portable
settings, and uninstall details.

## 🚀 Quick start

1. Run `CursorPeek.exe`.
2. Rest the pointer over a supported file in File Explorer.
3. Move off the file to dismiss the preview; right-click the tray icon for controls.

## 🗂️ Supported files

| Type | Supported extensions or filenames |
| --- | --- |
| **Images** | `.jpg`, `.jpeg`, `.jpe`, `.jfif`, `.png`, `.gif`, `.webp`, `.bmp`, `.dib`, `.ico`, `.tif`, `.tiff` |
| **Text, logs, and markup** | `.txt`, `.text`, `.log`, `.md` |
| **Data and configuration** | `.csv`, `.tsv`, `.json`, `.jsonc`, `.xml`, `.yaml`, `.yml`, `.toml`, `.ini`, `.cfg`, `.conf`, `.properties` |
| **C and C++** | `.c`, `.h`, `.cc`, `.cpp`, `.cxx`, `.hh`, `.hpp`, `.hxx`, `.ipp`, `.inl` |
| **Other source code** | `.rs`, `.cs`, `.java`, `.kt`, `.kts`, `.go`, `.py`, `.pyw`, `.rb`, `.php` |
| **Web, JavaScript, and TypeScript** | `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx`, `.html`, `.htm`, `.css` |
| **Scripts and queries** | `.sql`, `.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd` |
| **Exact filenames** | `README`, `LICENSE`, `COPYING`, `NOTICE`, `Makefile`, `Dockerfile`, `Gemfile`, `.env`, `.editorconfig`, `.gitattributes`, `.gitignore`, `.dockerignore`, `.npmrc`, `.prettierrc`, `.prettierignore`, `.eslintrc`, `.eslintignore` |

Matching is case-insensitive. Images and text are validated before previewing. See the
[format reference](docs/USER_GUIDE.md#supported-images) and
[known limitations](docs/KNOWN_LIMITATIONS.md) for behavior and limits.

## ⚙️ Settings

Right-click the tray icon to pause previews or change the dwell delay, maximum preview size, theme,
and startup behavior. See [settings and configuration](docs/USER_GUIDE.md#tray-menu).

## 🛡️ Privacy and security

CursorPeek runs locally without accounts, telemetry, file uploads, or application networking.
Read the [privacy policy](PRIVACY.md) and [security model](SECURITY.md) for details.

## 📚 Documentation

- [User guide](docs/USER_GUIDE.md)
- [Known limitations](docs/KNOWN_LIMITATIONS.md)
- [Privacy](PRIVACY.md)
- [Security](SECURITY.md)
- [Changelog](CHANGELOG.md)

## 🛠️ Build from source

Install the Rust version pinned in `rust-toolchain.toml` and Visual Studio Build Tools with the
**Desktop development with C++** workload:

```powershell
cargo build --locked --release
.\target\release\CursorPeek.exe
```

See the [development setup](CONTRIBUTING.md#development-setup) for tests and packaging.

## 🤝 Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Report vulnerabilities
privately through [SECURITY.md](SECURITY.md).

## 📜 License

CursorPeek is available under your choice of the
[Apache License 2.0](LICENSE-APACHE) or the [MIT License](LICENSE-MIT).
