"""剧场主题适配:日/夜模式下 data-color-scheme 与关键容器背景一致(安卓 UA)"""
import json
import pytest
from helpers import ev, info, body_bg, theme_var

def _lum(c):
    """相对亮度 0-1"""
    import re
    m = re.match(r"rgba?\((\d+),\s*(\d+),\s*(\d+)", c or "")
    if not m:
        return None
    vals = [int(v) / 255 for v in m.groups()]
    def lin(v):
        return v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = (lin(v) for v in vals)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b

def _scheme(pg):
    return ev(pg, "document.documentElement.getAttribute('data-color-scheme')")

def test_day_light_background(page):
    """日间模式:html 标记 day,正文背景为浅色"""
    assert _scheme(page) == "day", f"日间模式标记应为 day,实际 {_scheme(page)}"
    lum = _lum(body_bg(page))
    assert lum is not None and lum > 0.5, f"日间正文背景应浅, 亮度={lum} bg={body_bg(page)}"

def test_day_key_containers_light(page):
    """日间:魔法棒菜单背景浅色(米白),焦点区文字深色;(ghost/透明底控件不参与)"""
    page.click("#st-wand-btn", timeout=10000)
    page.wait_for_timeout(600)
    menu = info(page, "#st-wand-menu")
    assert menu and menu["w"] > 0, "魔法棒菜单应展开"
    lum = _lum(menu["bg"])
    assert lum is not None and lum > 0.5, f"魔法棒菜单 日间应浅色, bg={menu['bg']}"
    focus = info(page, "#st-focus-bar")
    assert focus and focus["w"] > 0, "焦点区应展开"
    tl = _lum(focus["color"])  # 前景文字色应深色(日间)
    assert tl is not None and tl < 0.5, f"焦点区日间文字应深色, color={focus['color']}"

def test_night_dark_background(page_night):
    """夜间模式:html 标记 night,正文背景为深色"""
    assert _scheme(page_night) == "night", f"夜间标记应为 night,实际 {_scheme(page_night)}"
    lum = _lum(body_bg(page_night))
    assert lum is not None and lum < 0.25, f"夜间正文背景应深, 亮度={lum} bg={body_bg(page_night)}"

def test_theme_differs_day_night(page, page_night):
    """日/夜切换:标记与背景亮度差异显著"""
    d, n = _scheme(page), _scheme(page_night)
    assert d != n, f"日夜标记应不同 d={d} n={n}"
    dl, nl = _lum(body_bg(page)), _lum(body_bg(page_night))
    assert dl is not None and nl is not None and (dl - nl) > 0.3, f"日夜背景亮度差应显著 d={dl} n={nl}"
