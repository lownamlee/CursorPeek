# CursorPeek privacy

CursorPeek is designed as a local, offline utility.

## Data handling

- CursorPeek does not require an account.
- CursorPeek does not contain telemetry, analytics, advertising, content upload, crash upload,
  update-check, or application networking behavior.
- Configuration remains on the current computer, either under
  `%LOCALAPPDATA%\CursorPeek\config.ini` or beside a portable executable.
- Raster images, SVG, and text files are opened only inside a separate worker after File Explorer
  identity and local-file checks succeed. SVG resources stay self-contained: CursorPeek does not
  resolve network URLs, local files, external fonts, or stylesheets from an SVG. For video
  playback, the main process reopens the validated local path, compares its exact volume/file
  identity, container family, and metadata with the worker result, and retains a
  no-write/no-delete lock while Windows Media Foundation uses the path.
- File content is used to produce the on-screen preview and bounded in-memory cache. CursorPeek does
  not create a content index or preview-history database.
- Unsupported, ambiguous, network, device, offline, and recall-on-access targets fail closed.

Windows, File Explorer, security software, a sync provider, or the user’s network environment may
perform their own activity independently of CursorPeek. In particular, Explorer or a provider may
hydrate a cloud file before CursorPeek examines its attributes.

## Sensitive files

Eligible text files can contain secrets. CursorPeek displays their content locally on screen,
including eligible files such as `.env`. Pause or exit CursorPeek while screen sharing or whenever
the display is not private.

Diagnostic and bug reports should not include private file contents, credentials, personal paths,
or undisclosed vulnerability details.

## Containment boundary

Raster, SVG, and text parsing and decoding occur in a bounded worker with authenticated IPC,
process mitigations, timeout recovery, and a kill-on-close Job. Video extension and container
eligibility are checked by that worker, but native video/audio decoding runs through Windows Media
Foundation in the main process. The worker still runs as the same Windows user. It is not a
least-privilege sandbox and should not be described as protection from every same-user compromise.

Security-sensitive behavior and private reporting instructions are documented in
[SECURITY.md](SECURITY.md).
