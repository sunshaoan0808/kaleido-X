"""剧场基础冒烟:进入、正文加载、顶栏/魔法棒/输入区关键控件(安卓 UA)"""
import json
import pytest
from helpers import ev, info
from conftest import BASE, FULL_SID

def test_enter_tavern_loads_story(page):
    """进入剧场会话:剧本文本已加载"""
    body = ev(page, "document.body.innerText")
    assert "较早对话已折叠" in body, "应加载剧本文本(88轮/176条预览)"
    assert "原版1-16" in body or "我失忆后" in body, "应显示剧本标题"

def test_key_controls_render(page):
    """顶栏资料、魔法棒、输入区:渲染且可见"""
    for sel, hint in [("#st-wand-btn", "魔法棒"), ("#st-drawer-toggle", "资料抽屉"), ("#st-composer", "输入区")]:
        el = info(page, sel)
        assert el, f"{sel} 应存在"
        assert el["w"] > 0 and el["h"] > 0, f"{hint}({sel}) 应渲染出尺寸 {el['w']}x{el['h']}"

def test_return_button_present(page):
    """顶栏返回按钮存在"""
    for sid in ["#imm-back", "#reader-back", "#page-back"]:
        if info(page, sid) and info(page, sid)["w"] > 0:
            return
    # 兜底:寻找返回语义按钮
    r = ev(page, """()=>{const els=[...document.querySelectorAll('button')];
      return els.filter(b=>/返回|back/i.test((b.title||'')+(b.getAttribute('aria-label')||'')+(b.innerText||'').slice(0,8))).map(b=>({id:b.id,cls:(b.className||'').slice(0,30)})).slice(0,5)}""")
    assert r, f"未找到顶栏返回按钮 {r}"

def test_topbar_title_only(page):
    """顶栏标题区域显示剧本名+章节(沉浸式),不混入'就绪'等噪音"""
    title = ev(page, """(()=>{const t=document.getElementById('imm-title')||document.getElementById('reader-title')||document.getElementById('st-status');
      return t?t.innerText.trim().slice(0,60):''})()""")
    assert title, "顶栏应有标题元素"
    assert "就绪" not in title, f"顶栏不应含'就绪'噪音,实际: {title}"

def test_android_ua(page):
    """确认安卓 UA + mobile UI"""
    r = ev(page, "({ua:!!navigator.userAgent.match(/Android/), vw:innerWidth, ui:document.documentElement.getAttribute('data-ui')})")
    assert r["ua"], f"应为安卓 UA,实际 {ev(page,'navigator.userAgent').slice(0,80)}"
    assert r["vw"] <= 500, f"应为移动视口,实际 {r['vw']}px"
    assert r["ui"] == "mobile", f"应为 mobile UI,实际 {r['ui']}"
