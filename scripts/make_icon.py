# Sayit 图标生成器：渐变圆角方底 + 白色声波条纹（"说出来"的声音可视化）
# 用法：uv run --with pillow scripts/make_icon.py
from PIL import Image, ImageDraw

S = 1024
TOP = (59, 91, 255)      # 靛蓝 #3B5BFF
BOTTOM = (47, 158, 110)  # 品牌绿 #2F9E6E（与 UI 主色一致）

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

# 垂直渐变背景，用圆角矩形做蒙版
grad = Image.new("RGBA", (S, S))
gd = ImageDraw.Draw(grad)
for y in range(S):
    t = y / S
    color = tuple(int(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3)) + (255,)
    gd.line([(0, y), (S, y)], fill=color)
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=232, fill=255)
img.paste(grad, (0, 0), mask)

# 五根白色声波条纹（中间高、两侧低——说话的节奏感）
d = ImageDraw.Draw(img)
heights = [140, 240, 330, 220, 120]
bw, gap = 78, 44
total = 5 * bw + 4 * gap
x0 = (S - total) // 2
cy = S // 2
for i, h in enumerate(heights):
    x = x0 + i * (bw + gap)
    y0 = cy - h // 2
    y1 = y0 + h
    d.rounded_rectangle([x, y0, x + bw - 1, y1], radius=bw // 2, fill=(255, 255, 255, 255))

img.save("scripts/icon-source.png")
print("已生成 scripts/icon-source.png")
