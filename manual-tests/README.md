# CursorPeek manual fixtures

This directory contains deterministic local files for checking CursorPeek in File
Explorer. The suite follows the format policies in
`crates/cursorpeek-core/src/sniff.rs`; the verifier fails when those policies and
the fixture coverage drift apart.

All visual content was generated for CursorPeek. The credential-like text files
contain deliberately fake values.

## 🖼️ Images

`images/` contains the original 480 × 300 baseline set and
`images/variants/` adds size, shape, animation, transparency, and multi-image
coverage.

Every supported raster extension is represented:

`jpg`, `jpeg`, `jpe`, `jfif`, `png`, `gif`, `webp`, `bmp`, `dib`, `ico`, `tif`,
and `tiff`.

| Scenario | Fixture |
|---|---|
| Minimum dimensions | `tiny-1x1.png` |
| Small natural-size preview | `small-64x40.webp` |
| Square | `square-512x512.jpg` |
| Portrait | `portrait-360x900.jpeg`, `portrait-640x960.tif` |
| Landscape and panorama | `landscape-hd-1280x720.jpe`, `panorama-1600x400.jfif` |
| Large and decode-stress | `large-2560x1440.png`, `ultra-hd-3840x2160.webp` |
| Transparency | `alpha-960x540.webp` |
| Animation | `animated-640x360.gif`, `animated-640x360.webp` |
| Multi-image formats | `multi-size.ico`, `multipage-1920x1080.tiff` |
| EXIF orientation | `orientation-exif-6.jpg` |
| Tall and wide uncompressed files | `tall-480x1200.bmp`, `wide-1200x480.dib` |

The animated fixtures are useful even while a release intentionally shows only a
deterministic still frame.

## 📝 Text and SVG

`text/extensions/` contains one fixture for every supported text extension.
`text/exact-names/` covers every exact filename policy, including dotfiles such
as `.env`.

Additional scenarios cover:

- UTF-8, UTF-8 BOM, UTF-16 LE/BE, UTF-32 LE/BE, Windows-1252, and Shift-JIS
- empty, single-line, long-line, many-line, mixed-newline, and multilingual text
- a binary payload disguised with a `.txt` extension

`svg/` keeps three focused SVG cases separate from the extension matrix:

- static shapes
- SMIL animation
- an external reference using the reserved `.invalid` domain

These cases remain useful if SVG moves between source-text and rendered-preview
providers in a future release.

## 🎞️ Videos

`videos/sample.mp4` is a deterministic three-second, 640 × 360, 24 FPS Remotion
composition. FFmpeg derives the other supported containers from that same
master.

Every supported video extension is represented:

`mp4`, `m4v`, `mov`, `mp4v`, `3g2`, `3gp`, `3gp2`, `3gpp`, `avi`, `asf`, and
`wmv`.

The fixtures are silent so a manual hover test cannot unexpectedly play audio.

## 🧰 Regenerate and verify

Requirements:

- Python 3 with Pillow
- Node.js and npm
- FFmpeg and FFprobe

From the repository root:

```powershell
python .\manual-tests\generators\generate_images.py
python .\manual-tests\generators\generate_text.py

Push-Location .\manual-tests\generators\remotion
npm ci
npm run render
Pop-Location

.\manual-tests\generators\generate_video_variants.ps1
python .\manual-tests\generators\verify_fixtures.py
```

The Remotion project pins its dependencies and uses only locally defined
graphics. Its browser runtime is provisioned by Remotion when needed.

## ✅ Manual pass

Run CursorPeek, open one fixture directory at a time in File Explorer, and check:

- small images preserve their natural size while larger images respect the
  configured maximum
- portrait, panorama, transparency, EXIF orientation, and multi-image cases
  render without distortion
- GIF and WebP follow the release's documented animation policy
- short text has no empty lower area, while long text remains bounded
- encodings decode cleanly and binary-looking text fails safely
- each video begins promptly, stays silent, and shows filename metadata
- moving between the target and preview follows the intended hover lifetime

This is an opt-in manual suite, not part of the fast default test run or fuzz
corpus.
