# Generated image corpus

This corpus is generated entirely by the Rust tests in
`src/worker/image/corpus.rs`. No downloaded or opaque binary fixture is committed.

`cases.tsv` is the reviewable inventory. Each row names the generated input class, expected
fail-closed or preview outcome, and whether the resulting BGRA payload is replayed through the
Direct2D preview window.

The corpus covers:

- all seven selected still-image decoders;
- alpha, downscaling, and high compressed-to-decoded expansion;
- supported-but-misleading and unsupported extensions;
- deterministic truncation for every selected format;
- corrupt or absent PNG data;
- axis, pixel-count, and decoded-byte resource limits.

All bytes are original test output built from solid colors, primitive dimensions, or manually
constructed format headers. There are no third-party fixture licensing requirements.
