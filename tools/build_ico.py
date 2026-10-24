"""Assemble cyclops.ico (16-256 px) from the rendered masters, plus a preview strip.
python build_ico.py <icon_dir> <ico_out> <png256_out>

Resizing goes through Pillow's premultiplied 'RGBa' mode so edges stay clean.
"""
import sys
from PIL import Image

src, ico_out, png_out = sys.argv[1], sys.argv[2], sys.argv[3]
master = Image.open(f"{src}/icon_1024.png").convert('RGBA')
small = Image.open(f"{src}/icon_small_256.png").convert('RGBA')


def resized(img, side):
    return img.convert('RGBa').resize((side, side), Image.LANCZOS).convert('RGBA')


sizes = [256, 128, 96, 64, 48, 40, 32, 24, 20, 16]
frames = [resized(small if s <= 32 else master, s) for s in sizes]
frames[0].save(ico_out, format='ICO', sizes=[(s, s) for s in sizes], append_images=frames[1:])
frames[0].save(png_out)

big = resized(master, 512)
strip = Image.new('RGBA', (512 + 24 + sum(s + 12 for s in sizes[1:]) + 12, 2 * (512 + 24)), (0, 0, 0, 0))
for row, bg in enumerate(((28, 28, 32, 255), (240, 240, 244, 255))):
    y0 = row * (512 + 24)
    strip.paste(Image.new('RGBA', (strip.width, 512 + 24), bg), (0, y0))
    strip.alpha_composite(big, (12, y0 + 12))
    x = 512 + 24
    for f in frames[1:]:
        strip.alpha_composite(f, (x, y0 + 12 + (512 - f.height) // 2))
        x += f.width + 12
strip.save(f"{src}/ico_preview.png")
print("ico ok")
