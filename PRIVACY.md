# CursorPeek privacy

CursorPeek is designed as a local, offline utility.

## Data handling

- CursorPeek does not require an account.
- CursorPeek does not contain telemetry, analytics, advertising, content upload, crash upload,
  update-check, or application networking behavior.
- Configuration remains on the current computer, either under
  `%LOCALAPPDATA%\CursorPeek\config.ini` or beside a portable executable.
- A preview file is opened only inside a separate worker after File Explorer identity and local-file
  checks succeed.
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

Parsing and decoding occur in a bounded worker with authenticated IPC, process mitigations, timeout
recovery, and a kill-on-close Job. The worker still runs as the same Windows user. It is not a
least-privilege sandbox and should not be described as protection from every same-user compromise.

Security-sensitive behavior and private reporting instructions are documented in
[SECURITY.md](SECURITY.md).
