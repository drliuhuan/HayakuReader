from __future__ import annotations

import io
import struct
from dataclasses import dataclass
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICONS_DIR = ROOT / "assets" / "icons"
PREVIEW_PATH = Path(
    r"C:\Users\drliu\.gemini\antigravity\brain\4ca93d0b-3a3c-46b5-bfb3-04f1196c5446\file-icons-preview.png"
)

EXTENSIONS = [
    "pdf",
    "epub",
    "txt",
    "azw3",
    "azw",
    "mobi",
    "doc",
    "docx",
    "md",
    "markdown",
    "html",
    "htm",
    "xhtml",
    "xml",
]

ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]

# Matches assets/logo.svg background fill="#285847"
BRAND_GREEN = (40, 88, 71, 255)
BRAND_GREEN_EDGE = (31, 70, 56, 255)
PAGE_TOP = (255, 255, 255, 255)
PAGE_BOTTOM = (244, 246, 248, 255)
PAGE_BORDER = (138, 143, 152, 255)
FOLD_FILL = (230, 234, 239, 255)
FOLD_SHADOW = (0, 0, 0, 32)
LINE_COLOR = (176, 182, 190, 255)
BANNER_SHADOW = (0, 0, 0, 58)
WHITE = (255, 255, 255, 255)

FONT_REGULAR_BOLD = [
    r"C:\Windows\Fonts\segoeuib.ttf",
    r"C:\Windows\Fonts\arialbd.ttf",
    r"C:\Windows\Fonts\calibrib.ttf",
]

FONT_CONDENSED_BOLD = [
    r"C:\Windows\Fonts\bahnschrift.ttf",
    r"C:\Windows\Fonts\arialbd.ttf",
    r"C:\Windows\Fonts\segoeuib.ttf",
]


@dataclass(frozen=True)
class LayoutSpec:
    page_l: float
    page_r: float
    page_t: float
    page_b: float
    fold: float
    stroke_frac: float
    ban_l: float
    ban_r: float
    ban_t: float
    ban_b: float
    ban_radius_frac: float
    ban_shadow_frac: float
    text_w_ratio: float
    text_h_ratio: float
    top_lines: tuple[float, ...]
    bottom_lines: tuple[float, ...]
    line_w_frac: float


def layout_for_size(target_size: int) -> LayoutSpec:
    if target_size >= 64:
        return LayoutSpec(
            page_l=0.145,
            page_r=0.855,
            page_t=0.045,
            page_b=0.955,
            fold=0.210,
            stroke_frac=0.015,
            ban_l=0.085,
            ban_r=0.915,
            ban_t=0.280,
            ban_b=0.550,
            ban_radius_frac=0.018,
            ban_shadow_frac=0.022,
            text_w_ratio=0.86,
            text_h_ratio=0.70,
            top_lines=(0.145, 0.210),
            bottom_lines=(0.640, 0.715, 0.790, 0.865),
            line_w_frac=0.013,
        )
    if target_size >= 32:
        return LayoutSpec(
            page_l=0.125,
            page_r=0.875,
            page_t=0.035,
            page_b=0.965,
            fold=0.185,
            stroke_frac=0.026,
            ban_l=0.035,
            ban_r=0.965,
            ban_t=0.220,
            ban_b=0.610,
            ban_radius_frac=0.024,
            ban_shadow_frac=0.024,
            text_w_ratio=0.91,
            text_h_ratio=0.78,
            top_lines=(0.135,),
            bottom_lines=(0.710, 0.805, 0.895),
            line_w_frac=0.022,
        )
    return LayoutSpec(
        page_l=0.110,
        page_r=0.890,
        page_t=0.030,
        page_b=0.970,
        fold=0.170,
        stroke_frac=0.042,
        ban_l=0.015,
        ban_r=0.985,
        ban_t=0.185,
        ban_b=0.675,
        ban_radius_frac=0.030,
        ban_shadow_frac=0.028,
        text_w_ratio=0.94,
        text_h_ratio=0.82,
        top_lines=(),
        bottom_lines=(0.785, 0.885),
        line_w_frac=0.036,
    )


def load_font(size: int, condensed: bool = False) -> ImageFont.FreeTypeFont:
    candidates = FONT_CONDENSED_BOLD if condensed else FONT_REGULAR_BOLD
    for candidate in candidates:
        path = Path(candidate)
        if path.is_file():
            font = ImageFont.truetype(str(path), size=size)
            if condensed and path.name.lower() == "bahnschrift.ttf":
                try:
                    font.set_variation_by_axes([700, 75])
                except Exception:
                    pass
            return font
    raise RuntimeError("No suitable bold TrueType font found in C:\\Windows\\Fonts")


def fit_label_font(
    draw: ImageDraw.ImageDraw,
    label: str,
    max_width: int,
    max_height: int,
    condensed: bool,
) -> ImageFont.FreeTypeFont:
    start_size = max(48, int(round(max_height * 1.55)))
    for size in range(start_size, 18, -2):
        font = load_font(size, condensed=condensed)
        left, top, right, bottom = draw.textbbox((0, 0), label, font=font)
        if (right - left) <= max_width and (bottom - top) <= max_height:
            return font
    return load_font(18, condensed=condensed)


def render_icon_frame(ext: str, target_size: int) -> Image.Image:
    scale = 4 if target_size >= 64 else max(4, 256 // target_size)
    size = target_size * scale
    spec = layout_for_size(target_size)

    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img, "RGBA")

    page_l = int(round(size * spec.page_l))
    page_r = int(round(size * spec.page_r))
    page_t = int(round(size * spec.page_t))
    page_b = int(round(size * spec.page_b))
    fold = int(round(size * spec.fold))
    stroke = max(2, int(round(size * spec.stroke_frac)))

    if target_size >= 24:
        shadow_offset = max(2, int(round(size * 0.012)))
        shadow_poly = [
            (page_l + shadow_offset, page_t + shadow_offset),
            (page_r - fold + shadow_offset, page_t + shadow_offset),
            (page_r + shadow_offset, page_t + fold + shadow_offset),
            (page_r + shadow_offset, page_b + shadow_offset),
            (page_l + shadow_offset, page_b + shadow_offset),
        ]
        draw.polygon(shadow_poly, fill=(0, 0, 0, 36))

    page_mask = Image.new("L", (size, size), 0)
    mask_draw = ImageDraw.Draw(page_mask)
    page_poly = [
        (page_l, page_t),
        (page_r - fold, page_t),
        (page_r, page_t + fold),
        (page_r, page_b),
        (page_l, page_b),
    ]
    mask_draw.polygon(page_poly, fill=255)

    grad = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    grad_draw = ImageDraw.Draw(grad)
    span = max(1, page_b - page_t)
    for y in range(page_t, page_b + 1):
        t = (y - page_t) / span
        r = int(round(PAGE_TOP[0] + (PAGE_BOTTOM[0] - PAGE_TOP[0]) * t))
        g = int(round(PAGE_TOP[1] + (PAGE_BOTTOM[1] - PAGE_TOP[1]) * t))
        b = int(round(PAGE_TOP[2] + (PAGE_BOTTOM[2] - PAGE_TOP[2]) * t))
        grad_draw.line([(page_l, y), (page_r, y)], fill=(r, g, b, 255))
    img.paste(grad, (0, 0), page_mask)

    if target_size >= 32:
        fold_shadow_drop = max(2, int(round(size * 0.022)))
        draw.polygon(
            [
                (page_r - fold, page_t + fold),
                (page_r, page_t + fold),
                (page_r, page_t + fold + fold_shadow_drop),
                (page_r - fold + fold_shadow_drop // 2, page_t + fold + fold_shadow_drop),
            ],
            fill=FOLD_SHADOW,
        )

    fold_poly = [
        (page_r - fold, page_t),
        (page_r, page_t + fold),
        (page_r - fold, page_t + fold),
    ]
    draw.polygon(fold_poly, fill=FOLD_FILL)

    draw.line(
        page_poly + [page_poly[0]],
        fill=PAGE_BORDER,
        width=stroke,
        joint="curve",
    )
    draw.line(
        [
            (page_r - fold, page_t),
            (page_r - fold, page_t + fold),
            (page_r, page_t + fold),
        ],
        fill=PAGE_BORDER,
        width=stroke,
        joint="curve",
    )

    line_l = int(round(page_l + size * 0.068))
    line_r_full = int(round(page_r - size * 0.068))
    line_r_top = int(round(page_r - fold - size * 0.048))
    line_w = max(2, int(round(size * spec.line_w_frac)))

    for y_frac in spec.top_lines:
        y = int(round(size * y_frac))
        if line_r_top > line_l:
            draw.line([(line_l, y), (line_r_top, y)], fill=LINE_COLOR, width=line_w)

    for y_frac in spec.bottom_lines:
        y = int(round(size * y_frac))
        draw.line([(line_l, y), (line_r_full, y)], fill=LINE_COLOR, width=line_w)

    ban_l = int(round(size * spec.ban_l))
    ban_r = int(round(size * spec.ban_r))
    ban_t = int(round(size * spec.ban_t))
    ban_b = int(round(size * spec.ban_b))
    ban_radius = max(2, int(round(size * spec.ban_radius_frac)))
    ban_shadow_h = max(2, int(round(size * spec.ban_shadow_frac)))

    draw.rounded_rectangle(
        [(ban_l + max(1, int(size * 0.006)), ban_t + ban_shadow_h), (ban_r, ban_b + ban_shadow_h)],
        radius=ban_radius,
        fill=BANNER_SHADOW,
    )
    draw.rounded_rectangle(
        [(ban_l, ban_t), (ban_r, ban_b)],
        radius=ban_radius,
        fill=BRAND_GREEN,
        outline=BRAND_GREEN_EDGE,
        width=max(2, stroke // 2),
    )

    label = ext.upper()
    use_condensed = (target_size <= 48 and len(label) >= 4) or (len(label) >= 6)
    inner_w = int(round((ban_r - ban_l) * spec.text_w_ratio))
    inner_h = int(round((ban_b - ban_t) * spec.text_h_ratio))
    font = fit_label_font(draw, label, inner_w, inner_h, condensed=use_condensed)
    left, top, right, bottom = draw.textbbox((0, 0), label, font=font)
    text_w = right - left
    text_h = bottom - top
    text_x = (ban_l + ban_r) / 2.0 - (left + text_w / 2.0)
    text_y = (ban_t + ban_b) / 2.0 - (top + text_h / 2.0)
    draw.text((text_x, text_y), label, font=font, fill=WHITE)

    return img.resize((target_size, target_size), Image.Resampling.LANCZOS)


def write_png_ico(frames: list[Image.Image], target: Path) -> None:
    """Write a standard Windows multi-resolution ICO containing PNG-encoded frames."""
    target.parent.mkdir(parents=True, exist_ok=True)
    encoded: list[tuple[int, int, bytes]] = []
    for frame in frames:
        w, h = frame.size
        buf = io.BytesIO()
        frame.save(buf, format="PNG", optimize=True)
        encoded.append((w, h, buf.getvalue()))

    header = struct.pack("<HHH", 0, 1, len(encoded))
    offset = 6 + 16 * len(encoded)
    entries = bytearray()
    payload = bytearray()
    for w, h, data in encoded:
        bw = 0 if w >= 256 else w
        bh = 0 if h >= 256 else h
        entries.extend(struct.pack("<BBBBHHII", bw, bh, 0, 0, 1, 32, len(data), offset))
        payload.extend(data)
        offset += len(data)

    target.write_bytes(header + entries + payload)


def inspect_ico_sizes(path: Path) -> list[tuple[int, int]]:
    data = path.read_bytes()
    reserved, icon_type, count = struct.unpack_from("<HHH", data, 0)
    assert reserved == 0 and icon_type == 1, f"Invalid ICO header in {path}"
    sizes = []
    for i in range(count):
        w, h = struct.unpack_from("<BB", data, 6 + i * 16)
        sizes.append((256 if w == 0 else w, 256 if h == 0 else h))
    with Image.open(path) as img:
        pil_sizes = sorted(img.info.get("sizes", set()))
        assert sorted(sizes) == pil_sizes, f"PIL ICO read-back mismatch in {path}: {pil_sizes}"
    return sizes


def build_contact_sheet(all_frames: dict[str, dict[int, Image.Image]], dest: Path) -> None:
    cols = 4
    rows = (len(EXTENSIONS) + cols - 1) // cols
    cell_w, cell_h = 300, 250
    pad = 20
    sheet_w = cols * cell_w + (cols + 1) * pad
    sheet_h = rows * cell_h + (rows + 1) * pad
    sheet = Image.new("RGBA", (sheet_w, sheet_h), (30, 33, 38, 255))
    draw = ImageDraw.Draw(sheet)
    caption_font = load_font(20)
    small_font = load_font(13)

    for idx, ext in enumerate(EXTENSIONS):
        row = idx // cols
        col = idx % cols
        x0 = pad + col * (cell_w + pad)
        y0 = pad + row * (cell_h + pad)
        draw.rounded_rectangle(
            [(x0, y0), (x0 + cell_w, y0 + cell_h)],
            radius=14,
            fill=(48, 52, 60, 255),
        )
        frames = all_frames[ext]
        sheet.alpha_composite(frames[128], (x0 + 18, y0 + 24))
        sheet.alpha_composite(frames[48], (x0 + 168, y0 + 28))
        draw.text((x0 + 224, y0 + 44), "48px", font=small_font, fill=(185, 192, 204, 255))

        sheet.alpha_composite(frames[32], (x0 + 176, y0 + 88))
        draw.text((x0 + 224, y0 + 96), "32px", font=small_font, fill=(185, 192, 204, 255))

        sheet.alpha_composite(frames[24], (x0 + 180, y0 + 132))
        draw.text((x0 + 224, y0 + 136), "24px", font=small_font, fill=(185, 192, 204, 255))

        sheet.alpha_composite(frames[16], (x0 + 184, y0 + 168))
        draw.text((x0 + 224, y0 + 168), "16px", font=small_font, fill=(185, 192, 204, 255))

        draw.text((x0 + 18, y0 + 206), f".{ext}", font=caption_font, fill=(230, 235, 242, 255))

    dest.parent.mkdir(parents=True, exist_ok=True)
    sheet.convert("RGB").save(dest, format="PNG")


def main() -> None:
    all_frames: dict[str, dict[int, Image.Image]] = {}
    for ext in EXTENSIONS:
        frames_by_size = {s: render_icon_frame(ext, s) for s in ICO_SIZES}
        all_frames[ext] = frames_by_size
        out = ICONS_DIR / f"{ext}.ico"
        write_png_ico([frames_by_size[s] for s in ICO_SIZES], out)
        sizes = inspect_ico_sizes(out)
        print(f"{out.name}: {out.stat().st_size} bytes, sizes={sizes}")
    build_contact_sheet(all_frames, PREVIEW_PATH)
    print(f"Preview saved: {PREVIEW_PATH}")


if __name__ == "__main__":
    main()
