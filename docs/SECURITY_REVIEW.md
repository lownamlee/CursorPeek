# CursorPeek 0.1 security review

Review date: 2026-07-27

This review covers the locked release dependency graph, fuzz-tool graph, unsafe Windows boundary,
worker IPC and file boundary, preview UI boundary, and release SBOM process. It complements the
durable claims and limitations in [../THREAT_MODEL.md](../THREAT_MODEL.md).

## Dependency and license review

- `cargo deny --locked --workspace check` passes for the all-feature
  `x86_64-pc-windows-msvc` graph.
- The same policy passes for `fuzz/Cargo.lock` on `x86_64-unknown-linux-gnu`.
- RustSec advisories, yanked packages, unknown registries, Git sources, wildcard requirements, and
  duplicate crate versions have no accepted exceptions.
- The generally allowed SPDX set is `0BSD`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `MIT`,
  `Unicode-3.0`, `Unlicense`, and `Zlib`.
- `fuzz/deny.exceptions.toml` narrowly allows `NCSA` for the pinned
  `libfuzzer-sys 0.4.13`; that harness dependency is not linked into the Windows release
  executable.
- Both workspace manifests declare `MIT OR Apache-2.0`. The fuzz harness now declares the same
  project license.
- The policy checks declared or detected license metadata. Packaging still has to include the
  applicable third-party notices; the check is not legal advice.

`cargo-deny 0.20.2` is the pinned policy implementation. Apart from the fuzz-only license
exception above, the policy contains no ignored advisory, license clarification,
duplicate-version skip, or source exception.

## SBOM review

`tools/New-ReleaseSbom.ps1` requires `cargo-cyclonedx 0.5.9` and emits CycloneDX 1.5 JSON for the
actual default-feature Windows binary graph, including build dependencies. It fixes the source
epoch, verifies that `Cargo.lock` does not change, and rewrites only workspace `path+file`
references into canonical Cargo package URLs.

The generator then rejects file URIs, drive paths, home-directory paths, duplicate component
references, unresolved dependency references, an unexpected workspace package, tool-version
drift, or a non-reproducible timestamp. Two consecutive local generations were byte-identical and
contained 51 component references and 51 dependency records. Hosted CI repeats the generation and
uploads only the sanitized SBOM.

## Unsafe-code review

The shared parsing core has `#![deny(unsafe_code)]`. The Windows binary has
`#![deny(unsafe_op_in_unsafe_fn)]`, and Clippy's `undocumented_unsafe_blocks` lint is denied for all
targets and features.

The review closed every previously undocumented unsafe block in production and tests. The
remaining unsafe operations are confined to:

- Win32/COM calls with live owned handles or interfaces and explicit apartment/thread rules;
- sized input/output buffers and terminated UTF-16 strings;
- window-procedure state pointers with stable allocation and ordered teardown;
- initialized Windows unions and callback payloads;
- handle conversion into Rust ownership exactly once.

No unsafe code exists in content sniffing, protocol parsing, payload parsing, layout arithmetic, or
the fuzz harness.

## IPC, file, and UI review

- IPC parsing rejects invalid magic/version/kind/flags/order/length, nonce mismatch, stale
  generation, trailing bytes, truncation, and allocation above the protocol cap.
- Worker launch uses explicit inherited handles, suspended Job assignment, required creation
  mitigations, post-creation mitigation queries, timeout termination, and zero-residue recovery.
- File opening rejects relative/UNC/device paths, non-disk and remote-protocol handles, offline and
  recall-on-access content, invalid final-path shapes, directories, and changed identities.
- Text and image pipelines enforce their format-specific size, arithmetic, decoding, and rendered
  payload bounds. Retained fuzz seeds and ordinary Windows replay cover malformed boundaries.
- Preview callbacks preserve Explorer focus, use no-activate/click-eating behavior, and invalidate
  stale or lifecycle-affected content. The Windows qualification gate retains real focus, click,
  DPI, topology, theme, and resume observations.

## Release decision

No unresolved security-policy exception is open for 0.1; the sole scoped license allowance is the
fuzz-only NCSA entry described above. Portable and installer packaging include the exact locked
third-party license files, internal and artifact checksums, and clean-source metadata. The hosted
distribution lifecycle relocates an already configured portable copy without changing installed
state, upgrades an older registered install with settings and user files preserved, stops a running
instance, and finishes a separate uninstall with zero product residue. The per-user installer uses
a hash-pinned NSIS compiler, carries the distribution's exact multi-license `COPYING` notice, and
removes only its explicit owned-file list. Release documentation discloses the unsigned status and
remaining scope limits. The maintainer accepted the manually tested 0.1 packages. The tag-only
workflow must still rebuild the exact source, repeat the security and package gates, verify the
draft asset digests, create provenance and SBOM attestations, and publish only as its final action.
