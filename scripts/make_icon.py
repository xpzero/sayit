# Sayit 图标生成器：渐变圆角方底 + 白色麦克风
# "说出来" = 听写本体，双色 #4F7CFF → #2DD4BF（蓝→青碧，无紫色）与 UI 品牌一致
# 用法：uv run --with pillow scripts/make_icon.py
import math

from PIL import Image, ImageDraw

S = 1024
TOP = (79, 124, 255)     # 天空蓝 #4F7CFF
BOTTOM = (45, 212, 191)  # 青碧 #2DD4BF

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

# 垂直渐变背景，圆角矩形蒙版
grad = Image.new("RGBA", (S, S))
gd = ImageDraw.Draw(grad)
for y in range(S):
    t = y / S
    color = tuple(int(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3)) + (255,)
    gd.line([(0, y), (S, y)], fill=color)
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=232, fill=255)
img.paste(grad, (0, 0), mask)

# 麦克风画在独立图层，画完按内容 bbox 重新居中（几何微调不破坏构图）
layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(layer)
WHITE = (255, 255, 255, 255)

# 胶囊主体
d.rounded_rectangle([392, 210, 632, 630], radius=120, fill=WHITE)

# U 形托架弧（圆头端点用小圆补齐）
cx, cy, r, w = 512, 420, 290, 56
d.arc([cx - r, cy - r, cx + r, cy + r], 30, 150, fill=WHITE, width=w)
for a in (30, 150):
    ex = cx + r * math.cos(math.radians(a))
    ey = cy + r * math.sin(math.radians(a))
    d.ellipse([ex - w / 2, ey - w / 2, ex + w / 2, ey + w / 2], fill=WHITE)

# 立杆 + 底座
d.rounded_rectangle([484, 710, 540, 780], radius=28, fill=WHITE)
d.rounded_rectangle([382, 800, 642, 856], radius=28, fill=WHITE)

mark = layer.crop(layer.getbbox())
img.paste(mark, ((S - mark.width) // 2, (S - mark.height) // 2), mark)

img.save("scripts/icon-source.png")
print("已生成 scripts/icon-source.png")
