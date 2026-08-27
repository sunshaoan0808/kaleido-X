# -*- coding: utf-8 -*-
"""st-recall-bar(语义记忆召回条)回归:非播放状态(进入页/故事馆列表)必须隐藏"""
import pytest
from conftest import make_page, SESSION_URL, BASE

def _bar_state(pg):
    return pg.evaluate("""()=>{const b=document.getElementById('st-recall-bar');
      if(!b) return {exists:false};
      const r=b.getBoundingClientRect();
      return {exists:true, hidden:b.classList.contains('hidden'), h:Math.round(r.height), t:Math.round(r.top)};}""")

@pytest.mark.parametrize("ua", ["day", "night"])
def test_recall_bar_hidden_on_entry(pw, browser, ua):
    pg = make_page(pw, browser, ua, wait_ms=8000)
    pg.goto(SESSION_URL, timeout=30000)
    pg.wait_for_timeout(2500)
    s = _bar_state(pg)
    assert s["exists"], "st-recall-bar 缺失"
    assert s["hidden"], f"进入页召回条应隐藏,实际 hidden={s['hidden']} h={s['h']}"
    assert s["h"] == 0, f"隐藏后不应占空间,实际 h={s['h']}"

def test_recall_bar_hidden_on_tavern_list(pw, browser):
    pg = make_page(pw, browser, "day", wait_ms=8000)
    pg.goto(BASE + "/web/#/tavern", timeout=30000)
    pg.wait_for_timeout(2500)
    s = _bar_state(pg)
    assert s["exists"] and s["hidden"] and s["h"] == 0
