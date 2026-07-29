# CursorPeek 0.2 known limitations

This document describes the deliberate scope boundaries of CursorPeek 0.2. A file that does not
preview is not necessarily an application failure: CursorPeek normally shows nothing when identity,
locality, format, or resource checks cannot be satisfied safely.

## Platform and Explorer

- CursorPeek 0.2 supports x64 Windows 10 22H2 and Windows 11. There is no x86 or ARM64 package.
- Previews work in Windows File Explorer. The desktop, file pickers, archive viewers, third-party
  file managers, browsers, and other list controls are outside the 0.2 scope.
- CursorPeek requires one unambiguous local filesystem item from the visible view in the Explorer
  window under the pointer. The window may be inactive, but the item must remain unobscured.
  Folders, virtual Shell items, and ambiguous or changing candidates do not preview.
- UNC paths, mapped or remote files, device paths, offline files, and cloud placeholders that still
  require recall or hydration are rejected. CursorPeek does not download content.

## Content and presentation

- Only the formats and exact filenames listed in the
  [user guide](USER_GUIDE.md) are eligible. MP4 is the only supported video container. PDF, Office
  documents, archives, standalone audio, other media containers, font, executable, and folder
  previews are not included.
- MP4 codec availability follows the Windows Media Foundation components installed on the system.
  Video controls and scrubbing are not shown; the hover preview loops until dismissed.
- Animated GIF files use the first composited frame. WebP uses a still preview, ICO uses a
  deterministic first image, and TIFF uses a deterministic first page.
- Text is inert plain text. CursorPeek does not render Markdown or HTML, apply syntax highlighting,
  follow links, execute scripts, or provide an editor.
- Large images and text are bounded or truncated to the documented limits. CursorPeek does not
  expose a control for bypassing those safety limits.
- The preview is temporary and non-interactive. It cannot be pinned, copied from, zoomed, scrolled,
  or navigated between pages or frames.
- Rejected files normally produce no popup or detailed reason. Run the documented diagnostics and
  use a small known-good local file when troubleshooting.

## Input and session coverage

- The retained automated evidence primarily exercises ordinary mouse input in an interactive local
  session. Touchpad, pen, and Remote Desktop behavior can depend on the specific device, driver, or
  session; device-specific Raw Input gaps should be reported.
- CursorPeek runs one coordinator per local interactive session. It is not designed as a service
  or for non-interactive/server sessions.

## Distribution and updates

- The 0.2 packages are not Authenticode-signed. Windows SmartScreen or security software may show
  an unknown-publisher warning. Verify the package with the published `SHA256SUMS.txt` and GitHub
  artifact attestation before running it.
- CursorPeek has no automatic updater or update notification. Updating requires downloading and
  installing or replacing a newer package manually.
- Moving a portable folder after enabling **Start with Windows** leaves the old executable path in
  the current-user startup value. Turn the option off before moving, then enable it again from the
  new location.

## Security boundary

- The contained worker is a crash, hang, and resource-recovery boundary. It runs as the same
  Windows user and is not an AppContainer, restricted token, separate account, or complete security
  sandbox.
- A process already running as the same user, a compromised Windows installation, malicious
  drivers, and executable replacement are outside the security boundary.
- Previewing an eligible secrets file displays its content on screen. Pause or exit CursorPeek
  before screen sharing or when another person can see the display.

See the [user guide](USER_GUIDE.md) for supported formats and troubleshooting, and the
[threat model](../THREAT_MODEL.md) for the complete security boundary.
