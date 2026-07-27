# Changelog

This file records user-visible changes to CursorPeek. Release dates are added only when the
corresponding version is published.

## Unreleased

## [0.1.0] - 2026-07-27

### Added

- Native hover previews in Windows File Explorer without a shell extension or external runtime.
- Bounded previews for JPEG, PNG, GIF, WebP, BMP/DIB, ICO, TIFF, and the documented text formats.
- Plain-text rendering with strict Unicode handling, optional legacy decoding, control-character
  sanitization, and explicit size and line limits.
- Tray controls for pause, dwell delay, preview size, theme, startup, version information, and
  exit.
- Installed and portable settings, per-user startup registration, and single-instance behavior.
- Portable ZIP and per-user installer packages with checksums, release metadata, licenses, and
  third-party notices.
- Windows 10 22H2 and Windows 11 qualification across supported DPI, theme, high-contrast, and
  multi-monitor scenarios.

### Security

- File identity is correlated with the active Explorer view and revalidated from the opened local
  file handle.
- Parsing and decoding run in a contained worker with authenticated bounded IPC, process
  mitigations, resource limits, timeouts, and kill-on-close cleanup.
- Release builds enforce ASLR, NX, Control Flow Guard, CET compatibility, a bounded Windows import
  surface, locked dependency policy, reproducible SBOM generation, and tag-built provenance.
- CursorPeek performs no telemetry, content upload, update check, or application networking.

### Known limitations

Version 0.1 is intentionally narrow. See
[Known limitations](docs/KNOWN_LIMITATIONS.md) before installing or running this release.

[0.1.0]: https://github.com/lownamlee/CursorPeek/releases/tag/v0.1.0
