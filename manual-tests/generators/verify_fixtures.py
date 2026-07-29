from __future__ import annotations

import codecs
import json
import re
import shutil
import subprocess
import xml.etree.ElementTree as ElementTree
from pathlib import Path

from PIL import Image


REPOSITORY = Path(__file__).resolve().parents[2]
MANUAL_ROOT = REPOSITORY / "manual-tests"
SNIFF_SOURCE = REPOSITORY / "crates" / "cursorpeek-core" / "src" / "sniff.rs"


def rust_string_array(source: str, name: str) -> list[str]:
    match = re.search(
        rf"pub const {re.escape(name)}:.*?=\s*&\[(.*?)\];",
        source,
        flags=re.DOTALL,
    )
    if match is None:
        raise RuntimeError(f"Could not find Rust array {name}")
    return re.findall(r'"([^"]+)"', match.group(1))


def require_equal(label: str, expected: set[str], actual: set[str]) -> None:
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing or unexpected:
        raise RuntimeError(
            f"{label} differs from the product policy: "
            f"missing={missing or 'none'}, unexpected={unexpected or 'none'}"
        )


def verify_images(expected_extensions: set[str]) -> tuple[int, int, int]:
    image_files = sorted(
        path
        for path in (MANUAL_ROOT / "images").rglob("*")
        if path.is_file() and path.suffix.removeprefix(".").lower() in expected_extensions
    )
    require_equal(
        "Image extension coverage",
        expected_extensions,
        {path.suffix.removeprefix(".").lower() for path in image_files},
    )

    minimum_dimension = 2**31 - 1
    maximum_dimension = 0
    animated_files: list[Path] = []
    for path in image_files:
        with Image.open(path) as image:
            minimum_dimension = min(minimum_dimension, image.width, image.height)
            maximum_dimension = max(maximum_dimension, image.width, image.height)
            frame_count = getattr(image, "n_frames", 1)
            if frame_count > 1:
                animated_files.append(path)
            for frame_index in range(frame_count):
                image.seek(frame_index)
                image.load()

    variants = MANUAL_ROOT / "images" / "variants"
    required_variants = {
        "tiny-1x1.png",
        "ultra-hd-3840x2160.webp",
        "portrait-360x900.jpeg",
        "panorama-1600x400.jfif",
        "alpha-960x540.webp",
        "animated-640x360.gif",
        "animated-640x360.webp",
        "multi-size.ico",
        "multipage-1920x1080.tiff",
    }
    missing_variants = sorted(
        name for name in required_variants if not (variants / name).is_file()
    )
    if missing_variants:
        raise RuntimeError(f"Missing required image variants: {missing_variants}")

    with Image.open(variants / "animated-640x360.gif") as image:
        if getattr(image, "n_frames", 1) < 2:
            raise RuntimeError("The GIF fixture is not animated.")
    with Image.open(variants / "animated-640x360.webp") as image:
        if getattr(image, "n_frames", 1) < 2:
            raise RuntimeError("The WebP fixture is not animated.")
    with Image.open(variants / "multipage-1920x1080.tiff") as image:
        if getattr(image, "n_frames", 1) < 2:
            raise RuntimeError("The TIFF fixture is not multipage.")
    with Image.open(variants / "alpha-960x540.webp") as image:
        if "A" not in image.getbands():
            raise RuntimeError("The alpha WebP fixture has no alpha channel.")
    with Image.open(variants / "multi-size.ico") as image:
        sizes = image.ico.sizes()
        expected_sizes = {
            (16, 16),
            (32, 32),
            (48, 48),
            (64, 64),
            (128, 128),
            (256, 256),
        }
        if sizes != expected_sizes:
            raise RuntimeError(f"ICO sizes differ: expected={expected_sizes}, actual={sizes}")

    return len(image_files), minimum_dimension, maximum_dimension


def verify_text(expected_extensions: set[str], expected_names: set[str]) -> tuple[int, int]:
    extension_directory = MANUAL_ROOT / "text" / "extensions"
    actual_extensions = {
        path.name.removeprefix("sample.")
        for path in extension_directory.iterdir()
        if path.is_file() and path.name.startswith("sample.")
    }
    require_equal("Text extension coverage", expected_extensions, actual_extensions)

    names_directory = MANUAL_ROOT / "text" / "exact-names"
    actual_names = {path.name for path in names_directory.iterdir() if path.is_file()}
    require_equal("Exact text-name coverage", expected_names, actual_names)

    encoding_directory = MANUAL_ROOT / "text" / "encodings"
    expected_boms = {
        "utf-8-bom.txt": codecs.BOM_UTF8,
        "utf-16-le.txt": codecs.BOM_UTF16_LE,
        "utf-16-be.txt": codecs.BOM_UTF16_BE,
        "utf-32-le.txt": codecs.BOM_UTF32_LE,
        "utf-32-be.txt": codecs.BOM_UTF32_BE,
    }
    for filename, bom in expected_boms.items():
        if not (encoding_directory / filename).read_bytes().startswith(bom):
            raise RuntimeError(f"{filename} does not start with its expected BOM.")
    if b"\x00" not in (encoding_directory / "binary-disguised-as-text.txt").read_bytes():
        raise RuntimeError("The binary-lookalike text fixture contains no NUL byte.")

    return len(actual_extensions), len(actual_names)


def verify_svg() -> int:
    svg_files = sorted((MANUAL_ROOT / "svg").glob("*.svg"))
    required = {"static-shapes.svg", "animated-shapes.svg", "external-reference.svg"}
    require_equal("SVG scenario coverage", required, {path.name for path in svg_files})
    for path in svg_files:
        root = ElementTree.parse(path).getroot()
        if not root.tag.endswith("svg"):
            raise RuntimeError(f"{path.name} has no SVG root element.")
    return len(svg_files)


def verify_videos(expected_extensions: set[str]) -> int:
    video_files = sorted((MANUAL_ROOT / "videos").glob("*"))
    video_files = [
        path
        for path in video_files
        if path.is_file() and path.suffix.removeprefix(".").lower() in expected_extensions
    ]
    require_equal(
        "Video extension coverage",
        expected_extensions,
        {path.suffix.removeprefix(".").lower() for path in video_files},
    )

    ffprobe = shutil.which("ffprobe")
    if ffprobe is None:
        raise RuntimeError("ffprobe is required to validate the generated videos.")

    creation_flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    for path in video_files:
        completed = subprocess.run(
            [
                ffprobe,
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "format=duration:stream=codec_name,width,height",
                "-of",
                "json",
                str(path),
            ],
            check=True,
            capture_output=True,
            text=True,
            creationflags=creation_flags,
        )
        probe = json.loads(completed.stdout)
        streams = probe.get("streams", [])
        duration = float(probe.get("format", {}).get("duration", 0))
        if len(streams) != 1 or duration <= 0:
            raise RuntimeError(f"{path.name} has no valid video stream.")
        if int(streams[0].get("width", 0)) <= 0 or int(streams[0].get("height", 0)) <= 0:
            raise RuntimeError(f"{path.name} has invalid dimensions.")

    return len(video_files)


def main() -> None:
    source = SNIFF_SOURCE.read_text(encoding="utf-8")
    image_extensions = set(rust_string_array(source, "IMAGE_EXTENSIONS"))
    video_extensions = set(rust_string_array(source, "VIDEO_EXTENSIONS"))
    text_extensions = set(rust_string_array(source, "TEXT_EXTENSIONS"))
    text_names = set(rust_string_array(source, "TEXT_NAMES"))

    image_count, minimum_dimension, maximum_dimension = verify_images(image_extensions)
    text_extension_count, text_name_count = verify_text(text_extensions, text_names)
    svg_count = verify_svg()
    video_count = verify_videos(video_extensions)

    print(
        "Fixture verification passed: "
        f"images={image_count} ({len(image_extensions)} extensions, "
        f"{minimum_dimension}px..{maximum_dimension}px), "
        f"text_extensions={text_extension_count}, exact_names={text_name_count}, "
        f"svg_scenarios={svg_count}, videos={video_count}."
    )


if __name__ == "__main__":
    main()
