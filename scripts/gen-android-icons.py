#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""从 PC 端 logo 重绘安卓全套启动图标(运行环境需 Pillow)。

源图标 src-tauri/icons/icon.ico 只有 32x32 一帧(蓝底 #2563EB + 白色下箭头),
直接放大会糊;本脚本按逐像素测得的几何(杆 x14-17/y6-17, 三角 (6,18)-(25,18)-尖点
(15.5,26),三角底边朝上、尖朝下)做超采样矢量重绘,输出:
  - mipmap-{mdpi..xxxhdpi}/ic_launcher.png + ic_launcher_round.png (传统图标)
  - 不生成自适应图标(anydpi-v26)——HyperOS/MIUI 对自适应前景的缩放渲染会改变
    视觉比例,传统全出血图标在 MIUI 下与 PC 端观感一致(仅加圆角遮罩)。
manifest 需要 android:icon="@mipmap/ic_launcher" android:roundIcon="@mipmap/ic_launcher_round"。
用法: python scripts/gen-android-icons.py   (仓库根运行)
"""
from PIL import Image, ImageDraw
import os

BLUE = (37, 99, 235, 255)  # 原版实测背景色 #2563EB
WHITE = (255, 255, 255, 255)
RES = 'android/app/src/main/res'
DENS = {'mdpi': 48, 'hdpi': 72, 'xhdpi': 96, 'xxhdpi': 144, 'xxxhdpi': 192}


def make_master(size=1024, ss=4):
    S = size * ss / 32.0
    img = Image.new('RGBA', (size * ss, size * ss), BLUE)
    d = ImageDraw.Draw(img)
    d.rectangle([14 * S, 6 * S, 18 * S, 18.5 * S], fill=WHITE)          # 杆
    d.polygon([(6 * S, 18 * S), (25 * S, 18 * S), (15.5 * S, 26 * S)], fill=WHITE)  # 三角(尖朝下)
    return img.resize((size, size), Image.LANCZOS)


def main():
    master = make_master()
    for name, s in DENS.items():
        od = os.path.join(RES, f'mipmap-{name}')
        os.makedirs(od, exist_ok=True)
        master.resize((s, s), Image.LANCZOS).save(f'{od}/ic_launcher.png')
        mask = Image.new('L', (s * 4, s * 4), 0)
        ImageDraw.Draw(mask).ellipse([0, 0, s * 4 - 1, s * 4 - 1], fill=255)
        mask = mask.resize((s, s), Image.LANCZOS)
        circ = Image.new('RGBA', (s, s), (0, 0, 0, 0))
        circ.paste(master.resize((s, s), Image.LANCZOS), (0, 0), mask)
        circ.save(f'{od}/ic_launcher_round.png')
    print(f'图标已生成 → {RES}/mipmap-*/')


if __name__ == '__main__':
    main()
