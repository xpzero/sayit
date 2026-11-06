# Sayit 图标生成器 · E 定稿「墨滴字」
# 米纸底 + 角落淡墨晕 + 手写 sa（Bradley Hand Bold）+ 朱砂一点
# 与宣纸文人风 UI 同一血统（米纸 / 墨 / 朱砂）
# 用法：uv run --with pillow scripts/make_icon.py
from PIL import Image, ImageDraw, ImageFilter, ImageFont

S = 1024
PAPER = (253, 246, 236)     # 宣纸 #FDF6EC
SUMI = (23, 26, 32)         # 墨 #171A20
SEAL = (198, 70, 47)        # 朱砂 #C6462F

FONT_CANDIDATES = [
    "/System/Library/Fonts/Supplemental/Bradley Hand Bold.ttf",
    "/System/Library/Fonts/Supplemental/ChalkboardSE.ttc",
]

paper = Image.new("RGBA", (S, S), (0, 0, 0, 0))
pd = ImageDraw.Draw(paper)
pd.rounded_rectangle([0, 0, S - 1, S - 1], radius=246, fill=PAPER + (255,))

# 角落淡墨晕（左下重、右上轻）——纸被墨养过的呼吸感
wash = Image.new("RGBA", (S, S), (0, 0, 0, 0))
wd = ImageDraw.Draw(wash)
wd.ellipse([-260, 700, 320, 1280], fill=SUMI + (30,))
wd.ellipse([740, -240, 1300, 320], fill=SUMI + (22,))
wash = wash.filter(ImageFilter.GaussianBlur(90))
paper = Image.alpha_composite(paper, wash)

# 手写 "sa"
font = None
for path in FONT_CANDIDATES:
    try:
        font = ImageFont.truetype(path, 640)
        print(f"字体：{path}")
        break
    except Exception:
        continue
assert font is not None, "没有可用的手写字体"

d = ImageDraw.Draw(paper)
bbox = d.textbbox((0, 0), "sa", font=font)
tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
tx = (S - tw) / 2 - bbox[0] - 42       # 略左，给朱砂点让位
ty = (S - th) / 2 - bbox[1] - 34       # 略上
d.text((tx, ty), "sa", font=font, fill=SUMI + (255,))

# 朱砂一点（字的右下方——i 上的那滴，话音刚落）
r = 36
cx = int(tx + bbox[0] + tw + 92)
cy = int(ty + bbox[1] + th - 48)
d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=SEAL + (255,))

# 圆角蒙版裁切（墨晕不许出纸边）
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=246, fill=255)
img.paste(paper, (0, 0), mask)

img.save("scripts/icon-source.png")
print("已生成 scripts/icon-source.png（E 墨滴字 · 米纸/墨/朱砂）")
