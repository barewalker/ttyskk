#!/usr/bin/env python3
"""ttyskk のアイコンを作る。**出来上がりは repo に置いてあるので、普段は走らせない。**

意匠を変えたいときだけ走らせる。要るのは pycairo と PangoCairo と Noto Sans CJK JP。

    python3 fcitx5/icons/generate.py

**文字はパスに落とす。** SVG の中に `<text>` を残すと、描く側にそのフォントが無いと
別の字形になるか、消える。glyph を輪郭として書き出せば、どこで描いても同じに見える。

出来上がるもの:

    fcitx-ttyskk.svg            入力メソッド一覧などに出す既定のアイコン (▽)
    fcitx-ttyskk-hiragana.svg   かな          (あ)
    fcitx-ttyskk-katakana.svg   カタカナ      (ア)
    fcitx-ttyskk-halfkana.svg   半角カタカナ  (半)
    fcitx-ttyskk-fullwidth.svg  全角英数      (Ａ)
    fcitx-ttyskk-latin.svg      直接入力      (A)

同じものを PNG でも書き出す。**fcitx5 の札 (トレイ) は環境によって SVG を描けない**
ことがあるため、既存の入力メソッドに倣って両方置く。
"""

import os
import sys

import cairo
import gi

gi.require_version("Pango", "1.0")
gi.require_version("PangoCairo", "1.0")
from gi.repository import Pango, PangoCairo  # noqa: E402

SIZE = 48  # 意匠の基準となる大きさ。SVG はこれを viewBox にする
PNG_SIZES = (16, 22, 24, 32, 48, 64)
FONT = "Noto Sans CJK JP Bold 32"

# 文字を置く枠と、その下の帯。**縁の余白は詰める** — 22 ピクセルの札では、余白より
# 字の大きさが読みやすさを決める。
GLYPH_BOX = (5, 3, 38, 33)  # x, y, w, h
UNDERLINE = (14, 39, 20, 4)  # x, y, w, h

# 端末を思わせる暗い角丸。fcitx5-skk の明るい青とは輪郭からして別物にする。
BG = (0x1F / 255, 0x23 / 255, 0x28 / 255)
EDGE = (0x3D / 255, 0x44 / 255, 0x4D / 255)
FG = (0xE6 / 255, 0xED / 255, 0xF3 / 255)
ACCENT = (0x3F / 255, 0xB9 / 255, 0x50 / 255)
# 直接入力 (変換しない状態) を沈めて見せるための色
MUTED = (0x7D / 255, 0x85 / 255, 0x90 / 255)

# (ファイル名の後ろ, 文字, 文字の色, 下線を引くか)
#
# **全角Ａと半角 A は字幅だけでは見分けが付かない。** 22 ピクセルの札ではほぼ同じ形に
# なる。直接入力は「ttyskk が変換に関わらない状態」なので、色を沈めて下線も引かない。
# 意味の違いがそのまま見た目の違いになり、小さくても取り違えない。
ICONS = [
    ("", None, ACCENT, False),  # ▽ は文字ではなく図形で描く
    ("-hiragana", "あ", FG, True),
    ("-katakana", "ア", FG, True),
    ("-halfkana", "半", FG, True),
    ("-fullwidth", "Ａ", FG, True),
    ("-latin", "A", MUTED, False),
]


def rounded_rect(cr, x, y, w, h, r):
    cr.new_sub_path()
    cr.arc(x + w - r, y + r, r, -1.5708, 0)
    cr.arc(x + w - r, y + h - r, r, 0, 1.5708)
    cr.arc(x + r, y + h - r, r, 1.5708, 3.1416)
    cr.arc(x + r, y + r, r, 3.1416, 4.7124)
    cr.close_path()


def measure(cr, text):
    """文字の輪郭 (ink) の大きさ。倍率を決めるのに使う。"""
    layout = PangoCairo.create_layout(cr)
    layout.set_font_description(Pango.FontDescription(FONT))
    layout.set_text(text, -1)
    ink, _logical = layout.get_extents()
    iw = ink.width / Pango.SCALE
    ih = ink.height / Pango.SCALE
    if iw <= 0 or ih <= 0:
        raise SystemExit(f"文字の大きさを測れませんでした: {text!r}")
    return layout, ink, iw, ih


def common_scale():
    """全部の字に共通の倍率。**いちばん大きい字が枠いっぱいになる**ところまで上げる。

    **倍率を字ごとに変えてはいけない。** 字ごとに枠いっぱいへ伸ばすと全角Ａと半角 A が
    同じ形になり、見分けが付かなくなる。かといって字送り (logical) を基準にすると、
    CJK フォントは行間を含む分だけ余白が大きく、**絵に対して字が小さくなりすぎる**。

    そこで、輪郭 (ink) で測ったうえで倍率は一つに揃える。いちばん背の高い字がちょうど
    収まる倍率を選べば、余白を残さず、字幅の違いも残る。
    """
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, SIZE, SIZE)
    cr = cairo.Context(surface)
    _x, _y, w, h = GLYPH_BOX
    limits = []
    for _suffix, text, _color, _underline in ICONS:
        if text is None:
            continue
        _layout, _ink, iw, ih = measure(cr, text)
        limits.append(min(w / iw, h / ih))
    return min(limits)


def draw_glyph(cr, text, color, scale):
    """文字を GLYPH_BOX の中央に、共通の倍率で置き、輪郭として塗る。"""
    layout, ink, iw, ih = measure(cr, text)
    x, y, w, h = GLYPH_BOX
    cr.save()
    # 拡大したあとに中央へ寄せる。ink の左上が原点に来るように引く。
    cr.translate(x + (w - iw * scale) / 2, y + (h - ih * scale) / 2)
    cr.scale(scale, scale)
    cr.translate(-ink.x / Pango.SCALE, -ink.y / Pango.SCALE)
    cr.set_source_rgb(*color)
    PangoCairo.layout_path(cr, layout)
    cr.fill()
    cr.restore()


def draw_triangle(cr, color):
    """SKK の見出し語の印 ▽。文字ではなく図形なのでフォントが要らない。"""
    cr.set_source_rgb(*color)
    cr.set_line_width(5)
    cr.set_line_join(cairo.LINE_JOIN_ROUND)
    cr.move_to(10, 13)
    cr.line_to(38, 13)
    cr.line_to(24, 38)
    cr.close_path()
    cr.stroke()


def draw(cr, text, color, underline, scale):
    # 背景
    rounded_rect(cr, 2, 2, SIZE - 4, SIZE - 4, 9)
    cr.set_source_rgb(*BG)
    cr.fill_preserve()
    cr.set_source_rgb(*EDGE)
    cr.set_line_width(1.5)
    cr.stroke()

    if text is None:
        draw_triangle(cr, color)
        return

    draw_glyph(cr, text, color, scale)
    # 端末のカーソルを思わせる帯。モードごとの版を一目で束ねる印でもある
    if underline:
        rounded_rect(cr, *UNDERLINE, 2)
        cr.set_source_rgb(*ACCENT)
        cr.fill()


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    scale = common_scale()
    for suffix, text, color, underline in ICONS:
        name = f"fcitx-ttyskk{suffix}"

        svg_path = os.path.join(here, f"{name}.svg")
        surface = cairo.SVGSurface(svg_path, SIZE, SIZE)
        surface.set_document_unit(cairo.SVG_UNIT_PX)
        cr = cairo.Context(surface)
        draw(cr, text, color, underline, scale)
        surface.finish()

        for px in PNG_SIZES:
            surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, px, px)
            cr = cairo.Context(surface)
            cr.scale(px / SIZE, px / SIZE)
            draw(cr, text, color, underline, scale)
            out = os.path.join(here, f"{name}-{px}.png")
            surface.write_to_png(out)
        print(f"作りました: {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
