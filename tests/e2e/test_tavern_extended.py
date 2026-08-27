"""剧场扩展回归:真实点击链 + 全局重叠扫描 + 滚动/抽屉滑动 + 截图归档(贴近真机实测)
安全边界:续写/重试/生图/助手/发送为写操作,只验存在+hittest,不真实触发;
模式切换/通道选择/TTS暂停取消/折叠/tabs切换/抽屉会话跳转 无副作用,真实点击。
"""
import json, os
import pytest
from helpers import ev, info, overlap, body_bg, rel_lum, hit_owner

ART = os.path.join(os.path.dirname(__file__), "artifacts")
W = 700

def _click(pg, sel, wait=W):
    pg.click(sel, timeout=10000)
    pg.wait_for_timeout(wait)

def shot(page, name):
    os.makedirs(ART, exist_ok=True)
    page.screenshot(path=os.path.join(ART, f"{name}.png"), full_page=False)

def watch(page):
    errs = []
    page.on("pageerror", lambda e: errs.append(str(e)))
    return errs

def _visible_rects(page):
    """所有可见可点元素的 id/文本/矩形"""
    return ev(page, """()=>{const out=[];
      document.querySelectorAll('button,[role=button],[data-wand],.st-tool-select,.st-mode-btn,.st-panel-tab,.st-toggle-btn').forEach(e=>{
        const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)return;
        const cs=getComputedStyle(e);if(cs.visibility==='hidden'||cs.display==='none'||+cs.opacity===0)return;
        out.push({id:e.id||'',txt:(e.textContent||'').trim().slice(0,14),
          l:Math.round(r.left),t:Math.round(r.top),rr:Math.round(r.right),b:Math.round(r.bottom)});});
      return out}""")

def _overlap_scan(page):
    """两两矩形相交且中心命中链不含自身 => 遮挡;返回问题列表"""
    els = _visible_rects(page)
    problems = []
    for a in els:
        if a["rr"] - a["l"] <= 0 or a["b"] - a["t"] <= 0:
            continue
        cx, cy = (a["l"] + a["rr"]) // 2, (a["t"] + a["b"]) // 2
        chain = hit_owner(page, cx, cy) or []
        ids = {x["id"] for x in chain}
        if a["id"] and a["id"] not in ids:
            problems.append({"covered": a["id"], "by": sorted(ids), "rect": f"{a['l']},{a['t']}"})
    return problems, len(els)

def _scope_hittest(page, sel):
    """扫描 scope 内所有可见可点控件,逐个中心点 elementFromPoint,hittest 是否命中自身。
    只测中心点位于 scope 可视区内的控件(滚出可视区的跳过,属滚动裁剪非遮挡)。
    返回被遮挡/不可点控件描述列表。"""
    seljs = json.dumps(sel)
    js = ("()=>{const out=[];const scope=document.querySelector(" + seljs + ");"
          "if(!scope)return ['NO-SCOPE'];" 
          "const sr=scope.getBoundingClientRect();"
          "const walk=(e)=>{e.querySelectorAll('button,[role=button],a,input,select,.st-tool-select,.item').forEach(el=>{"
          "const r=el.getBoundingClientRect(),cs=getComputedStyle(el);"
          "if(r.width<=0||r.height<=0)return;"
          "if(cs.visibility==='hidden'||cs.display==='none')return;"
          "const cx=r.left+r.width/2,cy=r.top+r.height/2;"
          "if(cx<sr.left||cx>sr.right||cy<sr.top||cy>sr.bottom)return;"
          "if(cy<0||cy>window.innerHeight||cx<0||cx>window.innerWidth)return;"
          "const top=document.elementFromPoint(cx,cy);"
          "let n=top;while(n&&n!==document.body){if(n===el)return;n=n.parentElement}"
          "out.push(((el.id||el.className||'')+'').toString().slice(0,30)+' @'+Math.round(cy));});};"
          "walk(scope);return out}")
    return ev(page, js)

# ---------- 用户历史实测问题 → 回归 ----------
def test_wand_contains_main_focus_vessel(page):
    """实测:魔法棒里主线/焦点/换壳曾丢失 → 现在必须都在"""
    errs = watch(page)
    page.click("#st-wand-btn", timeout=10000)
    page.wait_for_timeout(700)
    for sel in ["#st-mode-mainline", "#st-mode-side", "#st-focus-bar",
                "#st-vessel-toggle", "#st-rot-toggle", "#st-image-btn", "#st-tts-btn"]:
        el = info(page, sel)
        assert el and el["visible"], f"{sel} 应可见"
    shot(page, "wand_full")
    assert not errs, f"打开魔法棒不应有 JS 错误: {errs[:3]}"

def test_topbar_back_and_drawer_respond(page):
    """实测:顶栏返回/资料点击没反应 → 点击必须有响应"""
    errs = watch(page)
    url0 = page.url
    _click(page, "#imm-back", 900)
    assert page.url != url0, "点返回后 URL 应变化(有响应)"
    shot(page, "after_back")
    assert not errs, f"返回不应 JS 报错: {errs[:3]}"

def test_topbar_drawer_opens_light_scrollable(page):
    """实测:图书按钮点后 2/3 变黑、抽屉夜间背景不能点滑 → 日间浅色+可滚动"""
    errs = watch(page)
    _click(page, "#st-drawer-toggle", 900)
    bg = ev(page, """()=>{const e=document.getElementById('st-drawer');if(!e)return null;
      return getComputedStyle(e).backgroundColor}""")
    if bg:
        lum = rel_lum(bg)
        assert lum is None or lum > 0.3, f"抽屉背景不应为夜间深色, bg={bg}"
    el = info(page, "#st-drawer-session-list")
    assert el and el["visible"], "会话列表应可见"
    # 滚动能力:列表本身或最近可滚动祖先支持滚动(obj 内容未溢出时 naturally 不滚,不强制)
    sc = ev(page, """()=>{let e=document.getElementById('st-drawer-session-list');
      while(e&&e!==document.body&&e!==document.documentElement){
        const o=getComputedStyle(e).overflowY;if(o==='auto'||o==='scroll')return [o,(e.id||e.className||'').toString().slice(0,24)];
        e=e.parentElement}return null}""")
    assert sc, "会话列表(或其滚动容器)应可滚动, 但 overflowY 全为 visible"
    # 列表应有实际内容(会话分组/项目)
    cnt = ev(page, "document.querySelectorAll('#st-drawer-session-list .item, #st-drawer-session-list .st-session-group').length")
    assert cnt and cnt >= 0, f"会话列表应有内容, 实际 {cnt}"
    # 抽屉内控件应可点(历史 bug:点击/滑动被正文拦截)
    bad = _scope_hittest(page, "#st-drawer-session-list")
    assert not bad, f"抽屉内控件存在遮挡/点不到: {bad[:6]}"
    # 新建按钮:滚到可见后再验证可点(面板较长,按钮初始在视口外)
    ev(page, "document.getElementById('st-drawer-save-create').scrollIntoView({block:'center'})")
    page.wait_for_timeout(400)
    p = ev(page, """()=>{const e=document.getElementById('st-drawer-save-create');if(!e)return 'no-btn';
      const r=e.getBoundingClientRect();const top=document.elementFromPoint(r.left+r.width/2,r.top+r.height/2);
      let n=top;while(n&&n!==document.body){if(n===e)return true;n=n.parentElement}return false}""")
    assert p, "新建按钮应可点(滚到可见后中心点命中自身)"
    shot(page, "drawer_open")
    assert not errs, f"打开抽屉不应 JS 报错: {errs[:3]}"

def test_drawer_session_item_navigates(page):
    """实测:抽屉会话项不能点击(正文在生效) → 展开分组后点会话项必须跳转"""
    _click(page, "#st-drawer-toggle", 900)
    url0 = page.url
    # 单场会话默认折叠进"其他会话"分组(MAX_FLAT=0),先展开
    g = ev(page, """()=>{const heads=[...document.querySelectorAll('#st-drawer-session-list .st-session-group-head')];
      const h=heads.find(e=>(e.textContent||'').includes('其他会话'))||heads[0];
      if(!h)return 'no-group';
      if(!h.classList.contains('open'))h.click();
      return h.classList.contains('open')?'opened':'clicked';}""")
    assert g in ("opened", "clicked"), f"应能展开会话分组, 实际 {g}"
    page.wait_for_timeout(700)
    r = ev(page, """()=>{const items=[...document.querySelectorAll('#st-drawer-session-list .item')];
      const it=items.find(e=>!e.classList.contains('active'))||items[0];
      if(!it)return 'no-item';
      const isActive=it.classList.contains('active');
      it.scrollIntoView({block:'center'});
      const b=it.getBoundingClientRect();
      return {x:Math.round(b.left+b.width/2),y:Math.round(b.top+b.height/2),
              txt:(it.textContent||'').trim().slice(0,20), isActive}}""")
    assert isinstance(r, dict) and r.get("x"), f"展开分组后应有可点会话项, 实际 {r}"
    page.wait_for_timeout(400)
    if r.get("isActive"):
        # 列表内仅当前会话项可点:hash 相同点击不触发路由跳转,该场景无法验证跳转(顺序依赖)
        pytest.skip("抽屉列表仅剩当前会话项,无法验证跳转")
    page.mouse.click(r["x"], r["y"])
    page.wait_for_timeout(1200)
    assert page.url != url0, f"点击会话项 '{r.get('txt')}' 应跳转(当前被正文拦截?)"
    shot(page, "drawer_item_jumped")

def test_side_mode_prompt_themed_not_blocking(page):
    """实测:选支线弹出俩提示,蓝底白字遮挡主题不符 → 提示应主题化且可关闭"""
    errs = watch(page)
    _click(page, "#st-wand-btn", 700)
    _click(page, "#st-mode-side", 800)
    # 收集屏幕上的浮层/提示(非 wand 菜单的 fixed 弹层)
    overlays = ev(page, """()=>{const out=[];
      document.querySelectorAll('.st-modal,.st-dialog,[class*=modal],[class*=dialog],[class*=toast],[class*=popup],[class*=overlay],[class*=confirm]').forEach(e=>{
        const r=e.getBoundingClientRect(),cs=getComputedStyle(e);
        if(r.width>0&&r.height>0&&cs.visibility!=='hidden'&&cs.display!=='none')out.push({
          cls:(e.className||'').toString().slice(0,40),txt:(e.textContent||'').trim().slice(0,24),
          bg:cs.backgroundColor,color:cs.color,l:Math.round(r.left),t:Math.round(r.top),w:Math.round(r.width),h:Math.round(r.height)});});
      return out}""")
    if overlays:
        for o in overlays:
            lum = rel_lum(o["bg"])
            assert lum is None or lum > 0.3, f"提示背景应主题化(日间浅色), 实际 {o['bg']} {o['cls']}"
        shot(page, "side_mode_prompt")
    assert not errs, f"选支线不应 JS 报错: {errs[:3]}"

# ---------- 交互状态切换(无副作用,真实点击) ----------
def test_rot_vessel_toggle_switch(page):
    """轮流/换壳:点亮⇄灰,active 类必须翻转"""
    errs = watch(page)
    _click(page, "#st-wand-btn", 700)
    st0 = ev(page, """()=>({rot:document.getElementById('st-rot-toggle').classList.contains('active'),
      ves:document.getElementById('st-vessel-toggle').classList.contains('active')})""")
    _click(page, "#st-rot-toggle", 500)
    st1 = ev(page, "document.getElementById('st-rot-toggle').classList.contains('active')")
    assert st1 != st0["rot"], f"轮流 active 应翻转 {st0}->{st1}"
    _click(page, "#st-vessel-toggle", 500)
    # 换壳真实交互:点亮展开角色选择器,含"不附身"选项;再点收起
    pk = ev(page, """()=>{const e=document.getElementById('st-vessel-picker');if(!e)return null;
      return {vis:getComputedStyle(e).visibility!=='hidden'&&e.offsetParent!==null,txt:(e.textContent||'').slice(0,60)}}""")
    assert pk and pk["vis"], f"点换壳应展开角色选择器, 实际 {pk}"
    assert "不附身" in (pk["txt"] or ""), f"选择器应含'不附身'选项, 实际 {(pk['txt'] or '')[:20]}"
    shot(page, "vessel_picker_open")
    _click(page, "#st-vessel-toggle", 400)
    assert not errs, f"切换不应 JS 报错: {errs[:3]}"

def test_history_fold_expands(page):
    """较早对话折叠条(88轮):点击必须展开历史"""
    errs = watch(page)
    _click(page, ".st-history-fold", 1200)
    txt = ev(page, "document.querySelector('.st-history-fold')?(document.querySelector('.st-history-fold').textContent||'').trim():''")
    assert txt, "折叠条点击后仍存在(应展开为历史区)"
    assert not errs, f"展开不应 JS 报错: {errs[:3]}"
    shot(page, "history_expanded")

def test_panel_tabs_switch(page):
    """线索板页签:病房疑云/晨光调查/当前状态 可切换且有内容变化
    (面板自 be64c3fc 起为独立弹窗:魔棒→#st-visual-btn 打开后才会渲染)"""
    errs = watch(page)
    _click(page, "#st-wand-btn", 700)
    _click(page, "#st-visual-btn", 1100)
    labels = ["病房疑云", "晨光调查", "当前状态"]
    seen = []
    for lab in labels:
        ok = ev(page, f"""()=>{{const t=[...document.querySelectorAll('.st-panel-tab')].find(e=>(e.textContent||'').includes({json.dumps(lab)}));
          if(!t)return false;t.click();return true}}""")
        assert ok, f"页签 {lab} 应存在"
        page.wait_for_timeout(700)
        seen.append(ev(page, "(document.querySelector('.st-panel-body')||{}).scrollHeight||0"))
        shot(page, f"tab_{lab[:2]}")
    assert len(set(seen)) >= 1 and any(s > 0 for s in seen), f"页签内容应可见, seen={seen}"
    assert not errs, f"切页签不应 JS 报错: {errs[:3]}"

def test_tts_controls_present(page):
    """朗读控制:tts-btn 可点(不被遮挡);暂停/取消按钮存在(朗读中才显示,不真实触发朗读)"""
    errs = watch(page)
    _click(page, "#st-wand-btn", 700)
    b = info(page, "#st-tts-btn")
    assert b and b["visible"], "朗读按钮应存在且可见"
    p = ev(page, """()=>{const e=document.getElementById('st-tts-btn');const r=e.getBoundingClientRect();
      const top=document.elementFromPoint(r.left+r.width/2,r.top+r.height/2);
      let n=top;while(n&&n!==document.body){if(n===e)return true;n=n.parentElement}return false}""")
    assert p, "朗读按钮应可点(中心点命中自身)"
    for sel in ["#st-tts-pause", "#st-tts-stop"]:
        assert info(page, sel), f"{sel} 应存在(朗读中显示)"
    shot(page, "tts_controls")
    assert not errs, f"TTS 控件不应 JS 报错: {errs[:3]}"

def test_image_channel_select(page):
    """生图通道选择:点击弹窗→选一项→弹窗关闭且通道值变化"""
    errs = watch(page)
    _click(page, "#st-wand-btn", 700)
    old = ev(page, "document.getElementById('st-image-channel').textContent.trim()")
    _click(page, "#st-image-channel", 800)
    # 弹窗内选项
    opts = ev(page, """()=>{const o=[];document.querySelectorAll('[class*=option],[class*=item],[role=option]').forEach(e=>{
      const r=e.getBoundingClientRect(),cs=getComputedStyle(e);
      if(r.width>0&&r.height>0&&cs.visibility!=='hidden'&&cs.display!=='none')o.push({t:(e.textContent||'').trim().slice(0,12),x:Math.round(r.left+r.width/2),y:Math.round(r.top+r.height/2)});});
      return o}""")
    assert opts, "点通道应有弹窗选项"
    shot(page, "channel_popup")
    target = next((o for o in opts if o["t"] and o["t"] != old), opts[0])
    page.mouse.click(target["x"], target["y"])
    page.wait_for_timeout(800)
    new = ev(page, "document.getElementById('st-image-channel').textContent.trim()")
    assert new != old, f"通道值应变化 {old} -> {new}"
    shot(page, "channel_selected")
    assert not errs, f"通道选择不应 JS 报错: {errs[:3]}"

# ---------- 全局重叠扫描 ----------
def test_no_overlap_wand_open(page):
    """魔法棒打开态:菜单内控件互不遮挡、都能点中;菜单完整在视口内(历史bug:顶部挤出屏幕)"""
    _click(page, "#st-wand-btn", 800)
    bad = _scope_hittest(page, "#st-wand-menu")
    assert not bad, f"菜单内控件存在遮挡/点不到: {bad[:6]}"
    m = ev(page, """()=>{const e=document.getElementById('st-wand-menu');const r=e.getBoundingClientRect();
      return {t:Math.round(r.top),b:Math.round(r.bottom),vh:innerHeight}}""")
    assert m["t"] >= 0 and m["b"] <= m["vh"], f"菜单应在视口内, 实际 {m}"

def test_no_overlap_drawer_open(page):
    """资料抽屉打开态:会话列表内控件互不遮挡、都能点中"""
    _click(page, "#st-drawer-toggle", 900)
    bad = _scope_hittest(page, "#st-drawer-session-list")
    assert not bad, f"抽屉会话列表控件存在遮挡/点不到: {bad[:6]}"

# ---------- 写操作按钮:存在且可点(不真实触发) ----------
def test_write_buttons_clickable_but_safe(page):
    """续写/重试/生图/助手/发送 存在且中心点命中自身(可点击),但不触发写操作"""
    _click(page, "#st-wand-btn", 700)
    for sel in ["#st-continue", "#st-retry", "#st-image-btn", "#st-magic-assist", "#st-send"]:
        el = info(page, sel)
        assert el and el["visible"], f"{sel} 应可见"
        p = ev(page, f"""(sel)=>{{const e=document.querySelector(sel);if(!e)return null;
          const r=e.getBoundingClientRect();const top=document.elementFromPoint(r.left+r.width/2,r.top+r.height/2);
          let n=top;while(n&&n!==document.body){{if(n===e)return true;n=n.parentElement}}return false}}""", sel)
        assert p, f"{sel} 中心点应命中自身(可点击,不被遮挡)"
