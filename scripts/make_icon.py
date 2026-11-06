# Sayit 图标生成器：录音点词标 —— 墨蓝平涂底 + 暖白圆润字 "Sayit"
# 象形手法：i 的点 = 录音红点（珊瑚橙圆点 + 微辉光）——这个字正在被"说出来"
# 字体：Arial Rounded Bold（圆头圆润，和圆点同一语言）；i 用无点 ı（U+0131）占位
# 用法：uv run --with pillow scripts/make_icon.py
from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

S = 1024
INK_BG = (31, 40, 54)        # 墨蓝平涂 #1F2836
TEXT_C = (242, 237, 228)     # 暖白 #F2EDE4
DOT_C = (255, 90, 60)        # 录音点 #FF5A3C

FONT = "/System/Library/Fonts/Supplemental/Arial Rounded Bold.ttf"

img = Image.new("RGBA", (S, S), INK_BG + (255,))
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=232, fill=255)
img.putalpha(mask)

# ---- 词标宽度自适应（无点 ı，让位给录音点） ----
TEXT = "Sayıt"
measure = ImageDraw.Draw(Image.new("RGBA", (8, 8)))
size = 300
for _ in range(4):
    font = ImageFont.truetype(FONT, size)
    w = measure.textlength(TEXT, font=font)
    if abs(w - 800) < 6:
        break
    size = max(1, round(size * 800 / w))

x0, base = (S - w) / 2, 620
w_say = measure.textlength("Say", font=font)
w_i = measure.textlength("ı", font=font)

# 先画整词（阴影 + 暖白主体）
word = Image.new("L", (S, S), 0)
ImageDraw.Draw(word).text((x0, base), TEXT, font=font, fill=255, anchor="ls")

shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
sa = ImageChops.offset(word, 0, 10).point(lambda a: int(a * 0.4))
shadow.paste((8, 12, 20, 255), (0, 0), sa)
shadow = shadow.filter(ImageFilter.GaussianBlur(8))
img.alpha_composite(shadow)
img.paste(TEXT_C + (255,), (0, 0), word)

# ---- 象形点：录音红点替换 ı 的点，带同色微辉光 ----
stem_cx = x0 + w_say + w_i / 2
ascent, _ = font.getmetrics()
bb = measure.textbbox((0, 0), "ı", font=font)  # bbox y 相对顶线（ascender）
stem_top = base - (ascent - bb[1])  # 换算到画布坐标：字干顶端
r = size * 0.085
cy = stem_top - r * 1.35

glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(glow).ellipse(
    [stem_cx - r * 2.1, cy - r * 2.1, stem_cx + r * 2.1, cy + r * 2.1],
    fill=DOT_C + (120,),
)
img.alpha_composite(glow.filter(ImageFilter.GaussianBlur(14)))

d = ImageDraw.Draw(img)
d.ellipse([stem_cx - r, cy - r, stem_cx + r, cy + r], fill=DOT_C + (255,))

img.save("scripts/icon-source.png")
print(f"已生成 scripts/icon-source.png（字号 {size}，点径 {2 * r:.0f}px）")
