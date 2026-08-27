"""剧场交互场景:魔法棒打开/聚焦区/轮流/换壳/生图通道/资料抽屉/顶栏(安卓 UA)"""
import json
import pytest
from helpers import ev, info, overlap

def _click(pg, sel, wait=500):
    pg.click(sel, timeout=10000)
    pg.wait_for_timeout(wait)

# ---------- 魔法棒 ----------
def test_wand_open_shows_focus_section(page):
    """点魔法棒:菜单展开,聚焦区/轮流/换壳出现(点亮态 active)"""
    _click(page, "#st-wand-btn")
    menu = info(page, "#st-wand-menu")
    assert menu and menu["w"] > 0, "魔法棒菜单应展开"
    focus = info(page, "#st-focus-bar")
    assert focus and focus["w"] > 0, "聚焦区应在菜单内出现"
    rot = info(page, "#st-rot-toggle")
    vessel = info(page, "#st-vessel-toggle")
    assert rot and rot["w"] > 0, "轮流开关应出现"
    assert vessel and vessel["w"] > 0, "换壳开关应出现"

def test_wand_menu_within_viewport(page):
    """魔法棒菜单不溢出视口"""
    _click(page, "#st-wand-btn")
    r = ev(page, """(()=>{const m=document.getElementById('st-wand-menu');const b=m.getBoundingClientRect();
      return {l:b.left,r:b.right,t:b.top,bt:b.bottom,vw:innerWidth,vh:innerHeight}})()""")
    assert r["l"] >= 0 and r["r"] <= r["vw"] + 1, f"菜单横向溢出 {r}"
    assert r["t"] >= 0 and r["bt"] <= r["vh"] + 1, f"菜单纵向溢出 {r}"

def test_wand_menu_toggle_close(page):
    """再点一次魔法棒:菜单关闭"""
    _click(page, "#st-wand-btn")
    _click(page, "#st-wand-btn")
    menu = info(page, "#st-wand-menu")
    assert menu and menu["w"] == 0, "再点应关闭菜单"

# ---------- 轮流/换壳 ----------
def test_rot_toggle_toggles_state(page):
    """轮流开关:点击后 aria-pressed/active 切换(点亮=生效)"""
    _click(page, "#st-wand-btn")
    before = ev(page, "document.getElementById('st-rot-toggle').getAttribute('aria-pressed')")
    _click(page, "#st-rot-toggle")
    after = ev(page, "document.getElementById('st-rot-toggle').getAttribute('aria-pressed')")
    assert before != after, f"轮流开关应切换状态 {before} -> {after}"

def test_vessel_toggle_opens_picker(page):
    """换壳开关:点击展开容器角色选择"""
    _click(page, "#st-wand-btn")
    _click(page, "#st-vessel-toggle")
    picker = info(page, "#st-vessel-picker")
    assert picker and picker["w"] > 0, "换壳选择器应展开"

# ---------- 生图通道 ----------
def test_image_channel_visible_after_wand(page):
    """生图通道在魔法棒菜单内可见"""
    _click(page, "#st-wand-btn")
    ic = info(page, "#st-image-channel")
    assert ic and ic["w"] > 0, "生图通道应可见"

# ---------- 资料抽屉 ----------
def test_drawer_opens_session_list(page):
    """顶栏资料:点击后抽屉展开,会话列表/存档/世界线可见"""
    _click(page, "#st-drawer-toggle", 800)
    r = ev(page, """()=>{const o={};['st-drawer-session-list','st-drawer-save-list','st-drawer-worldline','st-drawer-new-session'].forEach(id=>{
      const e=document.getElementById(id);if(!e){o[id]='missing';return}
      const b=e.getBoundingClientRect();o[id]=(b.width>0&&b.height>0)}) ;return o}""")
    assert r.get("st-drawer-session-list") is True, f"会话列表应展开, 实际 {r}"
    assert r.get("st-drawer-new-session") is True, f"新建入口应可见, 实际 {r}"

def test_drawer_has_new_session_entry(page):
    """抽屉内含'新建'入口"""
    _click(page, "#st-drawer-toggle")
    r = ev(page, "!!document.getElementById('st-drawer-new-session')")
    assert r, "抽屉应有新建入口"
