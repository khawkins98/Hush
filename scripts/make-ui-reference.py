#!/usr/bin/env python3
"""
Generate two UI reference sheets (light + dark) for design review.

Layout:
  - DMG background image tiled as canvas texture
  - Header: app icon + title
  - 4-column grid of all screen screenshots with labels
  - Footer: "Hush UI Reference — v{version}"

Output: tmp/uxwalk/ui-reference-light.png
        tmp/uxwalk/ui-reference-dark.png

Usage:
    python3 scripts/make-ui-reference.py
"""

import os
import sys
import json
import textwrap
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont, ImageFilter

REPO = Path(__file__).parent.parent
SHOTS_DIR = REPO / "tmp" / "uxwalk"
ICON_PATH = REPO / "src-tauri" / "icons" / "128x128@2x.png"
BG_PATH = REPO / "src-tauri" / "assets" / "dmg-background.png"
OUT_DIR = SHOTS_DIR

# Read version from package.json
with open(REPO / "package.json") as f:
    VERSION = json.load(f)["version"]

# ---------- layout constants ----------
THUMB_W, THUMB_H = 380, 285      # main screenshot thumbnails
COLS = 4
GAP = 18                         # gap between cells
MARGIN = 48                      # outer left/right margin
LABEL_H = 28                     # height reserved below each thumb for its label
CELL_W = THUMB_W + GAP
CELL_H = THUMB_H + LABEL_H + GAP

HEADER_H = 180                   # icon + title strip
FOOTER_H = 60

# Only HUD is not 800×600; give it a proportional thumb
HUD_THUMB_W = int(THUMB_W * 0.36)   # ~137 px wide
HUD_THUMB_H = int(HUD_THUMB_W * 60 / 290)

ICON_SIZE = 120

# ---------- colour themes for the chrome (not the screenshots) ----------
THEMES = {
    "light": {
        "bg_tint": (255, 255, 255, 200),       # semi-transparent white overlay on bg
        "header_bg": (245, 245, 247, 240),
        "cell_bg": (255, 255, 255, 230),
        "shadow": (0, 0, 0, 35),
        "label_fg": (60, 60, 67),
        "title_fg": (29, 29, 31),
        "footer_fg": (120, 120, 128),
        "border": (210, 210, 215, 180),
    },
    "dark": {
        "bg_tint": (20, 20, 28, 220),
        "header_bg": (28, 28, 36, 240),
        "cell_bg": (38, 38, 48, 230),
        "shadow": (0, 0, 0, 90),
        "label_fg": (200, 200, 210),
        "title_fg": (240, 240, 250),
        "footer_fg": (130, 130, 150),
        "border": (60, 60, 80, 180),
    },
}

# ---------- label text for each shot ----------
LABELS = {
    "01-dictation-no-model":       "Dictation · no model",
    "02-dictation-perms-ok":       "Dictation · perms OK",
    "03-dictation-perms-denied":   "Dictation · perm denied",
    "04-first-run-modal":          "First-run onboarding",
    "08-history-empty":            "History · empty",
    "09-history-populated":        "History · populated",
    "10-history-row-delete-armed": "History · delete confirm",
    "11-history-clear-all-confirm":"History · clear all",
    "20-settings-general":         "Settings · General",
    "21-settings-model":           "Settings · Model",
    "22-settings-vocabulary":      "Settings · Vocabulary",
    "23-settings-replacements":    "Settings · Replacements",
    "24-settings-meeting":         "Settings · Meeting",
    "25-settings-permissions":     "Settings · Permissions",
    "26-settings-about":           "Settings · About",
    "30-hud":                      "HUD pill",
}

SHOT_ORDER = list(LABELS.keys())


def tile_background(bg_src: Image.Image, w: int, h: int) -> Image.Image:
    """Tile bg_src to fill w×h."""
    canvas = Image.new("RGBA", (w, h))
    bw, bh = bg_src.size
    for y in range(0, h, bh):
        for x in range(0, w, bw):
            canvas.paste(bg_src, (x, y))
    return canvas


def load_font(size: int, bold: bool = False):
    # Try system fonts; fall back to PIL's built-in bitmap font.
    candidates = [
        "/System/Library/Fonts/SFNSDisplay.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/Library/Fonts/Arial.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/SFPro.ttf",
        "/System/Library/Fonts/SFProDisplay-Regular.otf",
    ]
    for path in candidates:
        if os.path.exists(path):
            try:
                from PIL import ImageFont
                return ImageFont.truetype(path, size)
            except Exception:
                continue
    return ImageFont.load_default()


def rounded_rect_mask(w: int, h: int, r: int) -> Image.Image:
    mask = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(mask)
    d.rounded_rectangle([0, 0, w - 1, h - 1], radius=r, fill=255)
    return mask


def add_shadow(composite: Image.Image, x: int, y: int, w: int, h: int, color, blur=8):
    shadow_layer = Image.new("RGBA", composite.size, (0, 0, 0, 0))
    sd = ImageDraw.Draw(shadow_layer)
    sd.rectangle([x + 3, y + 4, x + w + 3, y + h + 4], fill=color)
    shadow_layer = shadow_layer.filter(ImageFilter.GaussianBlur(blur))
    composite = Image.alpha_composite(composite, shadow_layer)
    return composite


def make_sheet(theme_name: str):
    shots_dir = SHOTS_DIR / theme_name
    t = THEMES[theme_name]

    # Figure out canvas dimensions
    rows = (len(SHOT_ORDER) + COLS - 1) // COLS
    content_w = COLS * THUMB_W + (COLS - 1) * GAP
    canvas_w = content_w + 2 * MARGIN
    canvas_h = HEADER_H + rows * CELL_H + FOOTER_H

    # --- background: tile DMG background, then tint ---
    bg_src = Image.open(BG_PATH).convert("RGBA")
    canvas = tile_background(bg_src, canvas_w, canvas_h)

    # Apply theme tint overlay
    tint = Image.new("RGBA", (canvas_w, canvas_h), t["bg_tint"])
    canvas = Image.alpha_composite(canvas, tint)

    draw = ImageDraw.Draw(canvas)

    # --- header ---
    hdr = Image.new("RGBA", (canvas_w, HEADER_H), t["header_bg"])
    # subtle bottom border
    hdr_draw = ImageDraw.Draw(hdr)
    hdr_draw.line([(0, HEADER_H - 1), (canvas_w, HEADER_H - 1)], fill=t["border"])
    canvas.paste(hdr, (0, 0), hdr)

    # App icon in header
    icon = Image.open(ICON_PATH).convert("RGBA")
    icon = icon.resize((ICON_SIZE, ICON_SIZE), Image.LANCZOS)
    icon_x = MARGIN
    icon_y = (HEADER_H - ICON_SIZE) // 2
    canvas.paste(icon, (icon_x, icon_y), icon)

    # Title text
    font_title = load_font(36, bold=True)
    font_sub   = load_font(20)
    font_label = load_font(15)
    font_footer = load_font(14)

    draw = ImageDraw.Draw(canvas)
    tx = icon_x + ICON_SIZE + 24
    draw.text((tx, icon_y + 4),  "Hush", font=font_title, fill=t["title_fg"])
    draw.text((tx, icon_y + 48), f"UI Reference Sheet · {theme_name.capitalize()} Mode · v{VERSION}",
              font=font_sub, fill=t["label_fg"])
    draw.text((tx, icon_y + 80), "All screens captured via headless Playwright with mocked IPC.",
              font=font_label, fill=t["footer_fg"])

    # --- screenshots grid ---
    for idx, key in enumerate(SHOT_ORDER):
        col = idx % COLS
        row = idx // COLS

        cell_x = MARGIN + col * (THUMB_W + GAP)
        cell_y = HEADER_H + row * CELL_H + GAP // 2

        shot_path = shots_dir / f"{key}.png"
        if not shot_path.exists():
            print(f"  [warn] missing {shot_path.name}", file=sys.stderr)
            continue

        shot = Image.open(shot_path).convert("RGBA")

        if key == "30-hud":
            # HUD is tiny — centre it in the cell area
            thumb = shot.resize((HUD_THUMB_W, HUD_THUMB_H), Image.LANCZOS)
            # Create a placeholder card the same size as other cells
            card = Image.new("RGBA", (THUMB_W, THUMB_H), t["cell_bg"])
            px = (THUMB_W - HUD_THUMB_W) // 2
            py = (THUMB_H - HUD_THUMB_H) // 2
            card.paste(thumb, (px, py), thumb)
            thumb_to_paste = card
        else:
            thumb_to_paste = shot.resize((THUMB_W, THUMB_H), Image.LANCZOS)

        # Drop shadow
        canvas = add_shadow(canvas, cell_x, cell_y, THUMB_W, THUMB_H,
                            color=t["shadow"], blur=6)

        # Rounded-corner mask for the screenshot
        mask = rounded_rect_mask(THUMB_W, THUMB_H, 8)
        canvas.paste(thumb_to_paste, (cell_x, cell_y), mask)

        # Cell border
        draw = ImageDraw.Draw(canvas)
        draw.rounded_rectangle(
            [cell_x, cell_y, cell_x + THUMB_W - 1, cell_y + THUMB_H - 1],
            radius=8, outline=t["border"], width=1,
        )

        # Label below thumbnail
        label_y = cell_y + THUMB_H + 6
        draw.text((cell_x + 4, label_y), LABELS[key],
                  font=font_label, fill=t["label_fg"])

    # --- footer ---
    draw.text(
        (MARGIN, canvas_h - FOOTER_H + 18),
        f"Hush v{VERSION}  ·  github.com/khawkins98/Hush  ·  {theme_name} theme  ·  "
        "screenshots generated by npm run test:screenshots",
        font=font_footer,
        fill=t["footer_fg"],
    )

    # Flatten to RGB for final PNG
    bg_white = Image.new("RGB", canvas.size, (255, 255, 255))
    bg_white.paste(canvas, mask=canvas.split()[3])
    out_path = OUT_DIR / f"ui-reference-{theme_name}.png"
    bg_white.save(out_path, "PNG", optimize=True)
    print(f"  ✓  {out_path}  ({bg_white.size[0]}×{bg_white.size[1]})")
    return out_path


if __name__ == "__main__":
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    print("Building UI reference sheets…")
    for theme in ("light", "dark"):
        make_sheet(theme)
    print("Done.")
