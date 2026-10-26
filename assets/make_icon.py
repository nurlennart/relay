"""Renders Relay's icon: two remote windows, the front one with a terminal prompt.

Run `python -I assets/make_icon.py` from the repository root to regenerate:
  assets/icon.ico   multi-size icon embedded in Relay.exe
  assets/icon.png   256 px preview
  assets/icon.rgba  256 px raw RGBA for the window and taskbar icon
"""
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
MINT = (137, 222, 173, 255)
TOP = (40, 48, 58)
BOTTOM = (15, 18, 23)


def render(size: int, weight: float, layered: bool = True) -> Image.Image:
    """Draws at 4x and downsamples for smooth edges. `weight` thickens strokes."""
    s = size * 4
    image = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # Tile with a vertical graphite gradient, clipped to a rounded square.
    gradient = Image.new("RGBA", (s, s))
    pixels = gradient.load()
    for y in range(s):
        t = y / (s - 1)
        color = tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)) + (255,)
        for x in range(s):
            pixels[x, y] = color
    mask = Image.new("L", (s, s), 0)
    inset = round(s * 0.03)
    ImageDraw.Draw(mask).rounded_rectangle(
        (inset, inset, s - inset, s - inset), radius=round(s * 0.22), fill=255
    )
    image.paste(gradient, (0, 0), mask)
    draw = ImageDraw.Draw(image)
    # A faint top highlight gives the tile some depth.
    draw.rounded_rectangle(
        (inset, inset, s - inset, s - inset),
        radius=round(s * 0.22),
        outline=(255, 255, 255, 28),
        width=max(1, round(s * 0.008)),
    )

    stroke = round(s * 0.05 * weight)
    # The front window, shifted down-left to make room for a second session behind it.
    left, top, right, bottom = s * 0.16, s * 0.30, s * 0.76, s * 0.79
    if layered:
        # A dimmer window behind: another remote machine.
        offset = s * 0.085
        draw.rounded_rectangle(
            (left + offset, top - offset, right + offset, bottom - offset),
            radius=round(s * 0.07),
            outline=MINT[:3] + (110,),
            width=stroke,
        )
    # The front window's body hides the one behind it.
    middle = tuple(round(a + (b - a) * 0.55) for a, b in zip(TOP, BOTTOM)) + (255,)
    draw.rounded_rectangle(
        (left, top, right, bottom), radius=round(s * 0.07), fill=middle, outline=MINT, width=stroke
    )
    # Its title bar.
    bar = top + s * 0.12
    draw.line((left + stroke / 2, bar, right - stroke / 2, bar), fill=MINT, width=stroke)

    # The prompt: a chevron and a cursor.
    cx, cy = s * 0.30, (bar + bottom) / 2 + s * 0.01
    arm = s * 0.085
    chevron = [(cx, cy - arm), (cx + arm, cy), (cx, cy + arm)]
    draw.line(chevron, fill=MINT, width=stroke, joint="curve")
    for x, y in (chevron[0], chevron[2]):
        draw.ellipse((x - stroke / 2, y - stroke / 2, x + stroke / 2, y + stroke / 2), fill=MINT)
    cursor_y = cy + arm
    draw.line((s * 0.48, cursor_y, s * 0.62, cursor_y), fill=MINT, width=stroke)
    for x in (s * 0.48, s * 0.62):
        draw.ellipse(
            (x - stroke / 2, cursor_y - stroke / 2, x + stroke / 2, cursor_y + stroke / 2),
            fill=MINT,
        )
    return image.resize((size, size), Image.LANCZOS)


def main() -> None:
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    # Small sizes get heavier strokes and drop the second window, which would turn to noise.
    images = [
        render(size, 1.45 if size <= 32 else 1.15 if size <= 64 else 1.0, layered=size > 24)
        for size in sizes
    ]
    largest = images[-1]
    largest.save(HERE / "icon.png")
    (HERE / "icon.rgba").write_bytes(largest.tobytes())
    largest.save(HERE / "icon.ico", sizes=[(n, n) for n in sizes], append_images=images[:-1])
    # A contact sheet to review every size at once.
    sheet = Image.new("RGBA", (sum(sizes) + 10 * len(sizes) + 10, 276), (30, 30, 30, 255))
    x = 10
    for size, image in zip(sizes, images):
        sheet.paste(image, (x, 266 - size), image)
        x += size + 10
    sheet.save(HERE / "icon-sizes.png")


if __name__ == "__main__":
    main()
