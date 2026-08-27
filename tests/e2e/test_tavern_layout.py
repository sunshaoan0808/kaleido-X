"""剧场布局:关键元素不重叠/不溢出屏幕/不遮挡(安卓 UA 393px)"""
import json
import pytest
from helpers import ev, info, overlap

def test_no_horizontal_overflow(page):
    """整个文档不应横向滚动"""
    vw = ev(page, "window.innerWidth")
    sw = ev(page, "document.documentElement.scrollWidth")
    assert sw <= vw + 1, f"文档横向溢出: scrollWidth={sw} > innerWidth={vw}"

def test_key_elements_in_viewport(page):
    """顶栏资料/魔法棒/输入区在视口内"""
    for sel in ["#st-wand-btn", "#st-drawer-toggle", "#st-composer"]:
        el = info(page, sel)
        assert el and el["w"] > 0 and el["inView"], f"{sel} 应在视口内 {el and el['w']}"

def test_wand_menu_in_viewport(page):
    """魔法棒菜单在视口内且不遮挡输入区"""
    from playwright.sync_api import expect
    page.click("#st-wand-btn", timeout=10000)
    page.wait_for_timeout(500)
    r = ev(page, """(()=>{const m=document.getElementById('st-wand-menu');const b=m.getBoundingClientRect();
      return {l:b.left,r:b.right,t:b.top,bt:b.bottom,vw:innerWidth,vh:innerHeight}})()""")
    assert r["l"] >= -1 and r["r"] <= r["vw"] + 1, f"菜单横向溢出 {r}"
    covered, chain = overlap(page, "#st-composer")
    assert not covered, f"输入区被菜单/其它层覆盖, 命中链={chain}"

def test_focus_bar_not_covered(page):
    """打开魔法棒后,聚焦区(轮流/换壳)不被覆盖"""
    page.click("#st-wand-btn", timeout=10000)
    page.wait_for_timeout(500)
    covered, chain = overlap(page, "#st-focus-bar")
    assert not covered, f"聚焦区被覆盖, 命中链={chain}"

def test_menus_hidden_dont_block(page):
    """未打开魔法棒时,正文中心可命中消息区(无隐藏层挡交互)"""
    covered, chain = overlap(page, "#st-messages")
    assert not covered, f"正文被隐藏层覆盖, 命中链={chain}"
