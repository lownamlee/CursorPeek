from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


MANUAL_ROOT = Path(__file__).resolve().parents[1]
OUTPUT = MANUAL_ROOT / "images" / "variants"
FONT_REGULAR = Path(r"C:\Windows\Fonts\segoeui.ttf")
FONT_BOLD = Path(r"C:\Windows\Fonts\segoeuib.ttf")


def font(size: int, *, bold: bool = False) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    candidate = FONT_BOLD if bold else FONT_REGULAR
    if candidate.is_file():
        return ImageFont.truetype(str(candidate), max(8, size))
    return ImageFont.load_default()


def card(width: int, height: int, label: str, *, alpha: bool = False) -> Image.Image:
    if (width, height) == (1, 1):
        return Image.new("RGBA", (1, 1), (37, 99, 235, 128 if alpha else 255))

    image = Image.new("RGBA", (width, height), (12, 24, 38, 0 if alpha else 255))
    draw = ImageDraw.Draw(image, "RGBA")
    unit = max(1, min(width, height) // 24)

    for row in range(height):
        ratio = row / max(1, height - 1)
        draw.line(
            (0, row, width, row),
            fill=(
                round(20 + 22 * ratio),
                round(45 + 55 * ratio),
                round(78 + 80 * ratio),
                round(190 + 55 * ratio) if alpha else 255,
            ),
        )

    margin = max(2, unit * 2)
    radius = max(2, unit * 2)
    draw.rounded_rectangle(
        (margin, margin, width - margin - 1, height - margin - 1),
        radius=radius,
        fill=(9, 20, 32, 205 if alpha else 245),
        outline=(96, 165, 250, 255),
        width=max(1, unit // 2),
    )

    circle_radius = max(2, min(width, height) // 10)
    circle_x = margin * 2 + circle_radius
    circle_y = margin * 2 + circle_radius
    draw.ellipse(
        (
            circle_x - circle_radius,
            circle_y - circle_radius,
            circle_x + circle_radius,
            circle_y + circle_radius,
        ),
        fill=(52, 211, 153, 235),
    )

    title_size = max(8, min(width // max(8, len(label)), height // 7))
    detail_size = max(8, min(width // 28, height // 13))
    title_x = min(width - margin, circle_x + circle_radius + margin)
    title_y = max(margin, circle_y - title_size // 2)
    draw.text(
        (title_x, title_y),
        label,
        font=font(title_size, bold=True),
        fill=(245, 249, 255, 255),
    )

    rule_y = min(height - margin * 3, max(circle_y + circle_radius + margin, height // 2))
    draw.rounded_rectangle(
        (margin * 2, rule_y, width - margin * 2, rule_y + max(1, unit)),
        radius=max(1, unit // 2),
        fill=(102, 124, 145, 220),
    )
    detail = f"{width} x {height}  |  CursorPeek manual fixture"
    draw.text(
        (margin * 2, min(height - margin - detail_size, rule_y + margin)),
        detail,
        font=font(detail_size),
        fill=(203, 213, 225, 255),
    )
    return image


def save_jpeg(path: Path, image: Image.Image, *, quality: int = 88) -> None:
    image.convert("RGB").save(
        path,
        format="JPEG",
        quality=quality,
        optimize=True,
        progressive=True,
    )


def animated_gif(path: Path, width: int, height: int) -> None:
    frames: list[Image.Image] = []
    for index in range(12):
        frame = card(width, height, f"GIF FRAME {index + 1:02d}")
        draw = ImageDraw.Draw(frame, "RGBA")
        radius = max(6, min(width, height) // 18)
        x = radius + round((width - radius * 2) * index / 11)
        y = height - radius * 2
        draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=(251, 146, 60, 255))
        frames.append(frame.convert("P", palette=Image.Palette.ADAPTIVE, colors=128))
    frames[0].save(
        path,
        format="GIF",
        save_all=True,
        append_images=frames[1:],
        duration=[90] * len(frames),
        loop=0,
        disposal=2,
        optimize=True,
    )


def animated_webp(path: Path, width: int, height: int) -> None:
    frames: list[Image.Image] = []
    for index in range(12):
        frame = card(width, height, f"WEBP FRAME {index + 1:02d}", alpha=True)
        draw = ImageDraw.Draw(frame, "RGBA")
        radius = max(6, min(width, height) // 18)
        x = radius + round((width - radius * 2) * index / 11)
        y = height - radius * 2
        draw.ellipse(
            (x - radius, y - radius, x + radius, y + radius),
            fill=(244, 114, 182, 230),
        )
        frames.append(frame)
    frames[0].save(
        path,
        format="WEBP",
        save_all=True,
        append_images=frames[1:],
        duration=[90] * len(frames),
        loop=0,
        lossless=True,
        method=6,
    )


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)

    card(1, 1, "1 PX", alpha=True).save(OUTPUT / "tiny-1x1.png", format="PNG")
    card(64, 40, "WEBP").save(
        OUTPUT / "small-64x40.webp",
        format="WEBP",
        lossless=True,
        method=6,
    )
    save_jpeg(OUTPUT / "square-512x512.jpg", card(512, 512, "JPG"))
    save_jpeg(OUTPUT / "portrait-360x900.jpeg", card(360, 900, "JPEG"))
    save_jpeg(OUTPUT / "landscape-hd-1280x720.jpe", card(1280, 720, "JPE"))
    save_jpeg(OUTPUT / "panorama-1600x400.jfif", card(1600, 400, "JFIF"))
    card(2560, 1440, "LARGE PNG").save(
        OUTPUT / "large-2560x1440.png",
        format="PNG",
        optimize=True,
    )
    card(3840, 2160, "ULTRA HD WEBP").save(
        OUTPUT / "ultra-hd-3840x2160.webp",
        format="WEBP",
        quality=82,
        method=6,
    )
    card(960, 540, "ALPHA WEBP", alpha=True).save(
        OUTPUT / "alpha-960x540.webp",
        format="WEBP",
        lossless=True,
        method=6,
    )
    animated_webp(OUTPUT / "animated-640x360.webp", 640, 360)
    animated_gif(OUTPUT / "animated-640x360.gif", 640, 360)
    card(480, 1200, "TALL BMP").convert("RGB").save(
        OUTPUT / "tall-480x1200.bmp",
        format="BMP",
    )
    card(1200, 480, "WIDE DIB").convert("RGB").save(
        OUTPUT / "wide-1200x480.dib",
        format="BMP",
    )
    card(256, 256, "ICO").save(
        OUTPUT / "multi-size.ico",
        format="ICO",
        sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    card(640, 960, "TIF").convert("RGB").save(
        OUTPUT / "portrait-640x960.tif",
        format="TIFF",
        compression="tiff_lzw",
    )
    first_page = card(1920, 1080, "TIFF PAGE 1").convert("RGB")
    second_page = card(1080, 1920, "TIFF PAGE 2").convert("RGB")
    first_page.save(
        OUTPUT / "multipage-1920x1080.tiff",
        format="TIFF",
        compression="tiff_lzw",
        save_all=True,
        append_images=[second_page],
    )

    for path in sorted(OUTPUT.iterdir()):
        with Image.open(path) as image:
            frames = getattr(image, "n_frames", 1)
            print(f"{path.name}\t{image.width}x{image.height}\tframes={frames}\tbytes={path.stat().st_size}")


if __name__ == "__main__":
    main()
