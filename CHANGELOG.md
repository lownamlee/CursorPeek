# Changelog

This file records user-visible changes to CursorPeek. Release dates are added only when the corresponding version is published.

## Unreleased

### Added

- Preview `.svg` files as inert markup source text through the existing contained text provider.
- Extend text previews to documentation markup (`.markdown`, `.mdx`, `.rst`, `.adoc`, `.tex`), more
  structured data (`.json5`, `.jsonl`, `.ndjson`, `.plist`, `.config`, `.hcl`, `.tf`, `.tfvars`,
  `.proto`, `.graphql`), more languages (`.vb`, `.fs`, `.scala`, `.groovy`, `.swift`, `.dart`,
  `.lua`, `.r`), web tooling (`.vue`, `.svelte`, `.astro`, `.scss`, `.sass`, `.less`), PowerShell
  modules and manifests (`.psm1`, `.psd1`), Windows and .NET project files (`.sln`, `.csproj`,
  `.vbproj`, `.vcxproj`, `.props`, `.targets`, `.resx`, `.nuspec`, `.manifest`), other build files
  (`.cmake`, `.mk`, `.gradle`), and plain-text data (`.diff`, `.patch`, `.reg`, `.po`, `.srt`,
  `.vtt`, `.ics`).
- Preview PEM-armored key and certificate files (`.pem`, `.crt`, `.cer`, `.csr`, `.key`, `.pub`,
  `.ppk`, `.asc`) and extensionless OpenSSH key files.
- Recognize more exact filenames, including `AUTHORS`, `CONTRIBUTING`, `CHANGELOG`, `CODEOWNERS`,
  `VERSION`, `Rakefile`, `Procfile`, `Justfile`, `Jenkinsfile`, `.gitmodules`, and `.nvmrc`.
- Preview header-verified MPEG-4/QuickTime/3GPP (`.mp4`, `.m4v`, `.mov`, `.mp4v`, `.3g2`, `.3gp`,
  `.3gp2`, `.3gpp`), AVI (`.avi`), and ASF (`.asf`, `.wmv`) video containers through native
  Windows playback.

### Changed

- Reveal video as soon as its verified player and preview surface are ready instead of waiting on a
  fixed 400 ms preroll.
- Retain one balanced Media Foundation platform session across hover players instead of restarting
  the platform for every video.

### Security

- Document that key, certificate, and registry previews put secret material on the display, and that
  binary variants of newly eligible names still fail the content check.

## [0.2.2] - 2026-07-29

This patch shortens the preview wait and adds opt-in diagnostics for investigating Explorer performance.

### Added

- Add an optional diagnostic build with correlated timing across the coordinator and contained preview worker.
- Include a PowerShell summary tool for hover-to-visible latency, lifecycle failures, and dropped diagnostic records.

### Changed

- Reduce the default dwell delay to 50 ms and use a 25 ms re-show delay after leaving a preview.
- Offer 50 ms, 100 ms, and 250 ms dwell presets from the notification-area menu.
- Optimize the release executable for runtime speed while retaining full link-time optimization.

### Privacy

- Keep diagnostic tracing disabled in normal release builds.
- Bound diagnostic storage and exclude file contents, file names, complete paths, credentials, and environment values.

## [0.2.1] - 2026-07-29

This patch keeps CursorPeek visible through two common Explorer hover conflicts.

### Fixed

- Keep CursorPeek above a validated Explorer infotip without activating or modifying Explorer.
- Preserve an active preview while the pointer is over the part of the preview that overlaps its verified source item.

### Security

- Restrict infotip handling to a visible `tooltips_class32` window owned by the validated Explorer process and overlapping the active preview.
- Accept only CursorPeek's active preview root while the physical point remains inside the stored resolver-validated source rectangle. Clicks, wheel input, keyboard input, and unrelated windows still dismiss the preview.

## [0.2.0] - 2026-07-28

This release makes previews faster, more compact, and available from visible inactive Explorer windows.

### Added

- Preview supported files in an unobscured Explorer window even while another application remains active.
- Carry the verified Explorer-window identity through the contained-worker protocol and revalidate the same root and active Shell view throughout preview lifetime.
- Make the repeat-preview cache configurable, with a larger 128-entry default.

### Changed

- Size image previews from their natural dimensions and shrink only when they exceed the selected maximum.
- Measure text content so short files do not leave unused popup space, with metadata below both image and text previews.
- Reduce repeated-hover latency through worker prewarming, Shell-view reuse, and verified preview caching.
- Preserve the dwell deadline while the pointer moves within one Explorer item and use a brief re-show delay when moving directly to another item.

### Security

- Protocol version 9 binds each product request to the exact Explorer root admitted by the coordinator. Occlusion, pointer departure, root/view replacement, destruction, hiding, or minimization still fail closed.
- Foreground changes now trigger target-context revalidation instead of weakening or discarding the Explorer identity check.

## [0.1.1] - 2026-07-27

This patch release restores the notification-area menu when CursorPeek is right-clicked.

### Fixed

- Routed notification-area callbacks through CursorPeek's event loop so the tray menu opens when the icon is right-clicked.
- Used the correct mouse, keyboard, and cursor-position contracts when placing the tray menu, and returned focus to the notification area after the menu closes.

## [0.1.0] - 2026-07-27

### Added

- Native hover previews in Windows File Explorer without a shell extension or external runtime.
- Bounded previews for JPEG, PNG, GIF, WebP, BMP/DIB, ICO, TIFF, and the documented text formats.
- Plain-text rendering with strict Unicode handling, optional legacy decoding, control-character sanitization, and explicit size and line limits.
- Tray controls for pause, dwell delay, preview size, theme, startup, version information, and exit.
- Installed and portable settings, per-user startup registration, and single-instance behavior.
- Portable ZIP and per-user installer packages with checksums, release metadata, licenses, and third-party notices.
- Windows 10 22H2 and Windows 11 qualification across supported DPI, theme, high-contrast, and multi-monitor scenarios.

### Security

- File identity is correlated with the active Explorer view and revalidated from the opened local file handle.
- Parsing and decoding run in a contained worker with authenticated bounded IPC, process mitigations, resource limits, timeouts, and kill-on-close cleanup.
- Release builds enforce ASLR, NX, Control Flow Guard, CET compatibility, a bounded Windows import surface, locked dependency policy, reproducible SBOM generation, and tag-built provenance.
- CursorPeek performs no telemetry, content upload, update check, or application networking.

### Known limitations

Version 0.1 is intentionally narrow. See [Known limitations](docs/KNOWN_LIMITATIONS.md) before installing or running this release.

[0.2.2]: https://github.com/lownamlee/CursorPeek/releases/tag/v0.2.2
[0.2.1]: https://github.com/lownamlee/CursorPeek/releases/tag/v0.2.1
[0.2.0]: https://github.com/lownamlee/CursorPeek/releases/tag/v0.2.0
[0.1.1]: https://github.com/lownamlee/CursorPeek/releases/tag/v0.1.1
[0.1.0]: https://github.com/lownamlee/CursorPeek/releases/tag/v0.1.0
