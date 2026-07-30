# CursorPeek threat model

This model defines CursorPeek's security goals and limitations for version 0.2. It describes the
application that is built from this repository; it does not turn the contained worker into a
sandbox or make guarantees about a compromised Windows account.

## Security goals

CursorPeek should:

- preview only the local file that File Explorer identifies under the current physical pointer;
- fail closed for ambiguous, virtual, remote, offline, recalled, or changing targets;
- treat file content, Shell data, IPC, settings, and window messages as untrusted;
- keep malformed or expensive content from blocking the coordinator indefinitely;
- preserve Explorer focus and deliver the user's click to Explorer, not the preview;
- keep preview content and settings local, without application networking or telemetry;
- leave no worker process behind after timeout, replacement, or shutdown.

Availability of Windows, Explorer, and the current user session is not a security guarantee. A
preview may fail without weakening the fail-closed path or content policy.

## Trust assumptions

CursorPeek relies on the Windows kernel, the current user's Windows session, system COM and
graphics services, and the executable loaded from disk. Shell and UI Automation values are not
accepted as trusted file identity merely because Windows supplied them; the resolver correlates
independent evidence and the worker rechecks the opened handle.

A process already running as the same Windows user may read the user's files, alter configuration,
send window messages, replace the executable, inspect process memory, or terminate CursorPeek.
Defending against that process is outside this model. The worker runs as the same user and is
contained for failure and resource recovery, not isolated as least privilege.

## Trust boundaries

| Boundary | Untrusted input | Required validation |
|---|---|---|
| Explorer to resolver worker | HWNDs, UIA elements, rectangles, cached names, Shell views and paths | Exact Explorer process/class/root checks, item shape and bounds, active-view correlation, filesystem-path result, target-context revalidation |
| Filesystem to preview worker | Path text, links, metadata, attributes, bytes, dimensions and encodings | Drive-absolute input, handle-derived final DOS path, disk/local protocol, no offline/recall attributes, stable handle snapshot, extension plus magic/content checks, bounded reads and checked arithmetic |
| Coordinator to worker | Pipe frames, generation, Explorer-root identity, point, pointer-travel span, encoding policy, or a bounded animation source identity | Explicit inherited handles, fixed framing, exact target-root correlation, ordered spans, local-path and stable-file-identity checks, length limits, ordering, cryptographic session nonce and one active/latest-pending request policy per worker |
| Worker to coordinator | Status, metadata, text, dimensions, BGRA payload, frame delays and animation pixels | Nonce handshake, message kind/order, generation, exact payload lengths, UTF-8/scalar/control rules, image/frame/duration arithmetic and stale-result rejection |
| Windows UI to coordinator | Raw Input, hooks, broadcasts, timers and arbitrary window messages | Private message IDs, scalar-only internal messages, handle/context checks, bounded callbacks, panic barriers and generation invalidation |
| Settings and startup | INI bytes, values, unknown keys, Local AppData and HKCU Run state | UTF-8/size limits, canonical typed values, atomic replacement, quoted exact executable path and per-user registry scope |
| Build and distribution | Crates, build scripts, workflow actions and generated artifacts | Locked versions, pinned actions/tools, advisory/license/source policy, SBOM, PE import/resource/hardening gates and artifact hashes |

## Threats and controls

### Wrong-file preview and time-of-check/time-of-use changes

UI Automation proves that the point is inside an Explorer item-shaped control. Shell enumeration
must produce exactly one correlated active view and a filesystem path. The candidate's frame,
geometry, identity, target Explorer root/view context, and generation are checked again before
dispatch.
The coordinator also records the complete pre-preview pointer span. The worker requires that span
to fit the resolved item rectangle before opening the file, so motion within one item can retain a
fixed dwell deadline without qualifying a sweep across multiple items.

The worker opens the file with a stable handle, rejects non-disk and remote-protocol handles,
derives the final DOS path from that handle, captures file identity/size/timestamps, and reads from
the handle rather than reopening the display name. Ambiguity or a changed candidate produces no
preview.

### Malformed content and resource exhaustion

Only an explicit raster image, SVG, video, or text extension or special filename is eligible.
Raster image magic remains authoritative. Video also requires a matching bounded ISO Base Media
`ftyp`, RIFF/AVI, or ASF Header Object signature. Product caps bound file size, read prefixes,
dimensions, pixel count, decoded bytes, SVG source and render complexity, animation frames and
duration, text bytes, scalar count, lines, protocol frames, cache entries, and cache bytes. Checked
arithmetic precedes allocation and layout.

Raster, SVG, and text parsing and decoding occur in workers. The first visual result remains the
latency path. SVG uses a static Rust renderer with external resource resolution and system-font
features disabled; malformed SVG never falls through to a markup-text preview. For an eligible
GIF, animated WebP, or supported declarative SVG animation, a separate optional worker reopens the
bounded local path and requires the same normalized path, volume serial, file ID, size,
last-write time, format, and dimensions before decoding a complete bounded animation. Its result
is accepted only for the still-current generation and Explorer target, so slow animation work
cannot block the first frame or the next normal hover. The SVG animation path has no script engine,
network access, filesystem resource resolver, or wall-clock input and rejects active content,
entities, and external references. Video container structure, locality, identity, and resource
eligibility are checked in a worker. Immediately before native Windows Media Foundation playback,
the UI process repeats those checks and compares the container family and exact file identity with
the worker result. It retains a no-write/no-delete handle while the path-only media API is active,
preventing replacement of the validated file. One request runs at a time and only the newest
pending request is retained per worker. A private Job limits each worker to one process and 384
MiB, kills it when the Job closes, and supports forced timeout recovery. Workers retire after idle
expiry.

### Worker spoofing, stale data, and handle inheritance

The coordinator creates anonymous pipes and supplies an explicit inherited-handle list during
suspended process creation. It assigns the Job and creation mitigations before the initial thread
runs, verifies DEP, bottom-up/high-entropy ASLR, and extension-point-disable policy, then resumes.

A 128-bit nonce from the Windows system RNG authenticates the ready handshake. The protocol
validates magic, version, kind, flags, nonce, generation, lengths, order, trailing bytes, and clean
EOF. Results for an old generation or replaced worker are discarded.

The nonce prevents accidental or off-path pipe confusion; it is not a secret against a process
that can inspect the same user's memory or handles.

### Unsafe Win32 and COM boundaries

The platform-neutral core denies all unsafe Rust. The Windows crate denies unsafe operations
inside unsafe functions, and CI denies undocumented unsafe blocks across all targets. Each unsafe
block states the relevant pointer lifetime, buffer size, handle ownership, union state, thread,
COM apartment, or callback invariant.

Owned handles and COM interfaces provide deterministic cleanup. Stable boxed window state is
stored only while its HWND is live and is cleared before teardown. Window callbacks catch Rust
panics before they could unwind across the system ABI.

### Focus, click, and on-screen disclosure

The preview uses a no-activate window, no-activate positioning, and
`MA_NOACTIVATEANDEAT`. The resolver-validated item rectangle and its exact visible, unobscured
Explorer root remain prerequisites; Explorer does not need to own the foreground. Pointer motion
inside that rectangle preserves the active generation. Foreground changes revalidate the target
context. Leaving or covering the item, buttons, wheel input, Escape, selection/view changes,
lifecycle changes, or a newer generation dismiss or invalidate the preview.

Text is rendered inertly: markup, scripts, links, terminal escapes, controls, and bidirectional
formatting are not executed. Eligible files may still contain secrets, and displaying them during
screen sharing remains a user-visible disclosure risk.

### Supply chain and release artifacts

`Cargo.lock` and the fuzz lockfile are committed. `deny.toml` rejects unknown registries, Git
dependencies, wildcard requirements, duplicate crate versions, unapproved licenses, yanked
packages, and RustSec advisories without a reviewed exception. Release SBOM generation uses a
pinned CycloneDX tool and removes machine-local workspace paths before validating every dependency
reference.

The release PE must retain ASLR, NX, Control Flow Guard, CET compatibility, relocation/load
configuration, the approved icon/version resources, and the approved system-DLL import boundary.
Portable and installer packages are produced from the same checked payload with canonical
metadata, exact license files, internal checksums, and adjacent SHA-256 records. The installer is
current-user only, uses a hash-pinned NSIS compiler, synchronizes startup through CursorPeek's
owned registry command, and uninstalls an explicit owned-file list rather than recursively deleting
the installation directory.

## Residual risks and non-goals

- The worker is not AppContainer, a restricted token, a separate account, or a security sandbox.
- Same-user compromise, malicious drivers, a compromised OS/Explorer, and executable replacement
  are outside the boundary.
- Rust and process containment reduce parser risk but cannot prove that dependencies contain no
  unknown memory-safety or logic flaw.
- Advisory and license automation depends on published metadata and known databases; it is not a
  legal opinion or a malicious-source-code detector.
- Explorer or a sync provider may hydrate content before CursorPeek observes the final attributes.
- Version 0.2 packages are unsigned unless the release notes explicitly say otherwise.
- Touchpad, pen, and RDP behavior can vary with the specific device, driver, or session; it is not
  used as a claimed isolation control.

Security reports should follow [SECURITY.md](SECURITY.md).
