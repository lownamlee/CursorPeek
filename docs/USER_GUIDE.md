# CursorPeek user guide

CursorPeek shows a temporary preview after the pointer rests over a supported local file in
Windows File Explorer. It runs as a notification-area application and does not replace Explorer,
install a shell extension, or open a network service.

Read the [0.2 known limitations](KNOWN_LIMITATIONS.md) before installing or running CursorPeek.

## Start and exit

Run `CursorPeek.exe`. Its icon appears in the notification area. Windows may place the icon behind
the notification-area overflow button.

Open File Explorer and keep the pointer within a supported file item for the configured dwell delay
(50 ms by default). The pointer may move inside that item before or after the preview appears.
Moving directly to another file uses a brief re-show delay while the complete Explorer and file
checks run again. Leaving Explorer, scrolling, clicking, pressing Escape, changing the active view,
or covering the hovered item dismisses the preview. Activating another application by itself does
not prevent a preview while the pointer remains over an unobscured Explorer item.

CursorPeek allows one normal instance in the current interactive session. Starting it again asks
the existing instance to show its tray menu.

To stop it, right-click the tray icon and choose **Exit**. Ending only the contained worker process
does not exit CursorPeek; the application replaces that worker when needed.

## Tray menu

Right-click the CursorPeek icon:

- **Pause** / **Resume** — temporarily stops or resumes new previews. Pause is not persisted.
- **Settings > Dwell delay**
  - Fast: 50 ms
  - Standard: 100 ms
  - Relaxed: 250 ms
- **Settings > Maximum preview size**
  - Compact: 480 x 360
  - Standard: 640 x 480
  - Large: 800 x 600
- **Settings > Theme**
  - Use system colors
  - Light
  - Dark
- **Settings > Start with Windows** — adds or removes the current executable from the current
  user’s startup list.
- **About CursorPeek** — shows the product version.
- **Exit** — closes CursorPeek and its worker.

Setting changes are saved immediately. If saving fails, CursorPeek leaves the previous setting in
effect and shows an error.

## Supported images

Eligible extensions:

| Family | Extensions | Behavior |
| --- | --- | --- |
| JPEG | `.jpg`, `.jpeg`, `.jpe`, `.jfif` | Applies supported EXIF orientation |
| PNG | `.png` | Preserves alpha |
| GIF | `.gif` | Uses the first composited frame |
| WebP | `.webp` | Uses a still preview |
| Bitmap | `.bmp`, `.dib` | Bounded raster decode |
| Icon | `.ico` | Uses a deterministic first-image policy |
| TIFF | `.tif`, `.tiff` | Uses a deterministic first-page policy |

An eligible extension is only the first check. CursorPeek also validates the file signature and
image layout. A renamed or malformed file does not become a valid image because its extension is
supported.

Current image limits:

- file size: 128 MiB;
- source width or height: 20,000 pixels;
- source pixels: 40,000,000;
- decoded image bytes: 160 MiB;
- decoder allocation: 256 MiB;
- displayed preview bitmap: at most 960 x 720 pixels.

Images are never enlarged beyond their decoded size and are fitted without changing aspect ratio.
The popup follows the displayed image size, up to the selected maximum, with file details beneath
the image.

## Supported text

Eligible extensions:

```text
txt text log md markdown mdx rst adoc tex svg
csv tsv json jsonc json5 jsonl ndjson xml plist yaml yml toml ini cfg conf config
properties hcl tf tfvars proto graphql
rs c h cc cpp cxx hh hpp hxx ipp inl cs vb fs java kt kts scala groovy go swift
dart py pyw rb php lua r
js mjs cjs jsx ts mts cts tsx vue svelte astro html htm css scss sass less
sql sh bash zsh ps1 psm1 psd1 bat cmd
sln csproj vbproj vcxproj props targets resx nuspec manifest cmake mk gradle
pem crt cer csr key pub ppk asc
diff patch reg po srt vtt ics
```

Eligible exact filenames:

```text
README LICENSE COPYING NOTICE AUTHORS CONTRIBUTING CHANGELOG CODEOWNERS VERSION
Makefile Dockerfile Gemfile Rakefile Procfile Justfile Jenkinsfile
.env .editorconfig .gitattributes .gitignore .gitmodules .dockerignore
.npmrc .nvmrc .prettierrc .prettierignore .eslintrc .eslintignore
id_rsa id_dsa id_ecdsa id_ed25519 known_hosts authorized_keys
```

Filename matching is case-insensitive. A file must still pass the binary-content checks.

CursorPeek recognizes strict UTF-8 and BOM-marked UTF-8, UTF-16, and UTF-32. When Unicode does not
apply, the default `auto` policy may select a supported legacy encoding. Guessed legacy text is
identified in the preview. Invalid sequences, binary-looking content, and unsupported encodings
fail closed.

`.svg` belongs to this text group, not to the image group. CursorPeek shows the SVG markup as
source text and never rasterizes it, so no vector renderer runs, no font is resolved, and no
referenced script, image, or stylesheet is fetched. Compressed `.svgz` is gzip rather than markup
and is not eligible.

An eligible extension never implies a parser. Project, patch, registry, subtitle, property-list,
and certificate files are read as bytes and decoded as text; nothing in them is interpreted. Binary
variants of these names — a `bplist00` property list, a DER-encoded `.cer`, a `.pfx` container —
fail the content check and show nothing.

Text is normalized and displayed as inert plain text:

- HTML, SVG, and Markdown are not rendered;
- scripts and terminal escape sequences are not executed;
- unsafe control and bidirectional formatting characters are neutralized;
- tabs are retained and hard line endings are normalized;
- output is bounded to 128 KiB of UTF-8, 32,000 Unicode scalars, and 200 lines;
- only a bounded 256 KiB prefix is read for a text preview.

The popup measures the rendered text and shrinks to fit short or empty files. The selected preview
size remains the maximum for longer content, and file details appear beneath the text.

Previewing a sensitive text file displays its content on your screen. This includes `.env`, private
keys and certificates (`.pem`, `.key`, `.ppk`, `.asc`, `id_rsa` and friends), and `.reg` exports.
CursorPeek is local and does not upload that content, but hovering such a file puts it on the
display: pause or exit CursorPeek when screen sharing, recording, presenting, or when other people
can see the screen. **Pause** is the quickest option and is not persisted, so it lasts only until
you resume or restart CursorPeek.

## Files that intentionally do not preview

CursorPeek requires one unambiguous local filesystem file from the visible view in the Explorer
window under the pointer. It does not preview:

- folders or multiple/ambiguous candidates;
- UNC and network paths;
- device paths and non-disk handles;
- offline files and placeholders marked for recall-on-open or recall-on-data-access;
- Explorer virtual items without a verified local path;
- unsupported extensions or special filenames;
- malformed, oversized, binary-disguised-as-text, or otherwise rejected content.

For these cases CursorPeek normally shows nothing. This is intentional fail-closed behavior, not
always an application error.

Explorer or a sync provider may have hydrated a file before CursorPeek examines it. CursorPeek
does not request hydration itself and rejects a target that still advertises an offline/recall
state when the worker opens it.

## Installed and portable settings

### Installed mode

Without a portable marker, settings are stored at:

```text
%LOCALAPPDATA%\CursorPeek\config.ini
```

The **Start with Windows** option writes the current executable command to the current user's
`Software\Microsoft\Windows\CurrentVersion\Run` registry key under the value `CursorPeek`. It does
not require administrator rights.

The per-user installer uses `%LOCALAPPDATA%\Programs\CursorPeek` by default and does not request
administrator rights. Its component page can add Start Menu shortcuts, a desktop shortcut, and
Start with Windows. Start Menu shortcuts are selected by default; the desktop and startup options
are opt-in.

Running the installer again repairs or upgrades the same current-user installation. Existing
`config.ini` settings and files not owned by CursorPeek are preserved. Uninstall removes only
CursorPeek's packaged files, shortcuts, startup registration, and empty directories. **Remove user
settings** is selected on the uninstall component page; clear it before continuing if you want to
keep `config.ini` for a later reinstall.

### Portable mode

The portable ZIP already contains an empty `CursorPeek.portable` file beside `CursorPeek.exe`.
Extract the complete folder before starting CursorPeek; its adjacent `.sha256` file verifies the
downloaded archive. A manually assembled copy can enable the same mode by creating a regular file
with that name. A directory with that name is rejected.

Portable mode does not copy files, modify PATH, or write the registry merely by starting. Choosing
**Start with Windows** is an explicit request to add the current portable executable to the
current-user startup key.

Keep the executable directory writable if you want settings to persist. Move the executable,
marker, and configuration together when relocating a portable copy.

### Configuration keys

The canonical file contains:

```ini
# CursorPeek settings
dwell_delay_ms=50
preview_width=640
preview_height=480
cache_entries=128
theme=system
legacy_encoding=auto
start_with_windows=false
```

| Key | Accepted value |
| --- | --- |
| `dwell_delay_ms` | integer from 50 through 2000 |
| `preview_width` | maximum preview client width, from 320 through 960 |
| `preview_height` | maximum preview client height, from 240 through 720 |
| `cache_entries` | retained preview count from 0 through 512; `0` disables caching |
| `theme` | `system`, `light`, or `dark` |
| `legacy_encoding` | `auto`, `system`, `off`, or a supported legacy label such as `windows-1252` or `shift_jis` |
| `start_with_windows` | `true` or `false` |

The tray exposes safe presets rather than every numeric value. To use another accepted value,
including a different cache capacity, exit CursorPeek, edit `config.ini` as UTF-8, and restart it.
Cached preview payloads remain independently capped at 64 MiB.

Malformed UTF-8, duplicate keys, invalid known values, oversized lines/files, or too many unknown
settings prevent startup and do not overwrite the existing file. Valid bounded unknown keys are
preserved for forward compatibility.

## Privacy and containment

CursorPeek has no account, telemetry, content upload, update check, or application network
protocol. It reads the selected local file only after Explorer identity checks succeed. Parsing
and decoding occur in a separate worker process with strict resource limits, authenticated bounded
IPC, process mitigations, and kill-on-close Job containment.

The worker runs as the same Windows user. It is not an AppContainer, a different integrity level,
or a security sandbox. Containment is designed to recover from decoder crashes, hangs, malformed
messages, and excess resource retention; it cannot promise isolation from every same-user process
compromise.

See [../PRIVACY.md](../PRIVACY.md) and [../SECURITY.md](../SECURITY.md).

## Diagnostics

Run commands from PowerShell or Command Prompt so their output remains visible.

Show version and help:

```powershell
.\CursorPeek.exe --version
.\CursorPeek.exe --help
```

Measure Raw Input coverage for one labeled input device:

```powershell
.\CursorPeek.exe --input-diagnostics
```

The command runs for 30 seconds. Keep File Explorer in the foreground and move the chosen mouse,
touchpad, or pen. An unmatched movement is a candidate coverage gap, not by itself proof that the
device is unsupported.

Verify contained-worker reuse, idle restart, and teardown:

```powershell
.\CursorPeek.exe --worker-diagnostics
```

Other command-line modes mentioned by `--help` are private test interfaces and are not supported
for normal use.

## Troubleshooting

### The tray icon is not visible

- Check the notification-area overflow.
- Start `CursorPeek.exe` once more; a second launch asks the existing instance to show its menu.
- If no instance exists, run it from PowerShell to capture the error text.
- An invalid or unwritable configuration can stop startup. Use the configuration recovery steps
  below.

### A supported file shows no preview

- Confirm the file is in Windows File Explorer, not a browser, desktop replacement, file dialog,
  archive viewer, or another file manager.
- Confirm it is a local file and does not need cloud hydration.
- Check that CursorPeek is not paused.
- Keep the pointer within one file item for at least the selected dwell delay.
- Verify the extension or exact filename is eligible.
- Try a small known-good file. Malformed, binary, oversized, ambiguous, or changing files are
  intentionally rejected.
- Exit other development/test copies and start only the build you intend to test.

### The preview disappears immediately

Leaving or covering the hovered item, wheel input, clicks, Escape, selection/view changes, display
changes, and Explorer lifecycle events dismiss it by design. Switching foreground applications
alone does not. Ordinary pointer motion inside the same item preserves the dwell deadline and the
visible preview.

### OneDrive or another cloud file does not preview

Make the file locally available through its provider before hovering. CursorPeek will not download
or hydrate it and will still reject files that advertise offline or recall attributes.

### Settings prevent startup

1. Exit CursorPeek.
2. Locate the active `config.ini` in the executable directory (portable) or
   `%LOCALAPPDATA%\CursorPeek` (installed).
3. Rename the file so it can be recovered.
4. Start CursorPeek to create defaults.
5. Reapply settings through the tray or copy back only valid key/value pairs.

Do not replace the portable marker with a directory.

### Start with Windows points to an old location

Start the intended copy, turn **Start with Windows** off, then turn it on again. This rewrites the
current-user startup value with the exact current executable path.

### High contrast or theme colors look different

High contrast takes precedence over the selected light/dark theme and uses Windows system colors.
This is intentional. System font and DPI changes apply when Windows sends the corresponding
notifications.

### Reporting a bug

Include:

- CursorPeek version or full commit;
- Windows version/build and Explorer view;
- portable or installed mode;
- display scaling and monitor layout;
- input device;
- minimal file type/size and whether it is local, linked, or cloud-managed;
- exact non-sensitive diagnostic output and reproduction steps.

Remove personal paths, file contents, credentials, and undisclosed vulnerability details. Use
[SECURITY.md](../SECURITY.md) for security reports.
