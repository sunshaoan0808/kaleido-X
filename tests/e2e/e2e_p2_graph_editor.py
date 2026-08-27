#!/usr/bin/env python3
"""P2 节点图画线编辑器 E2E（mock API）：档案馆→详情→剧本图→渲染→编辑→保存断言"""
import json, sys
from playwright.sync_api import sync_playwright

BASE = "http://127.0.0.1:18766"
HERE = "${REPO:-.}/tests/e2e"

NODES = [
    {"id": "n1", "chapterId": "ch1", "title": "开局", "entry": "你醒来。", "exit": [{"id": "x1", "when": "选择:出发", "next": "n2"}]},
    {"id": "n2", "chapterId": "ch1", "title": "出发", "entry": "向门外走。", "exit": [{"id": "x2", "when": "选择:东行", "next": "n3"}, {"id": "x3", "when": "选择:西行", "next": "n4"}]},
    {"id": "n3", "chapterId": "ch2", "title": "东行线", "entry": "林间小道。", "exit": []},
    {"id": "n4", "chapterId": "ch2", "title": "西行线", "entry": "荒原。", "exit": []},
]
PACK = {
    "id": "demo", "title": "测试剧本",
    "chapters": [{"id": "ch1", "title": "第一章", "bodyPath": "ch1.md"}, {"id": "ch2", "title": "第二章", "bodyPath": "ch2.md"}],
    "nodes": NODES, "blurb": "mock", "cast": [], "maxTier": "standard",
    "entry": {"startNodeId": "n1"}, "language": "zh",
}

def main():
    ls = json.load(open(HERE + "/fixtures/ls_real.json", encoding="utf-8"))
    try:
        app = json.loads(ls["kaleido_appearance_v1"])
    except Exception:
        app = {}
    app["mode"] = "day"; app["syncServer"] = False
    ls["kaleido_appearance_v1"] = json.dumps(app, ensure_ascii=False)

    out = {"ok": False}
    with sync_playwright() as p:
        b = p.chromium.launch(headless=True, executable_path="/usr/bin/google-chrome-stable")
        ctx = b.new_context(viewport={"width": 1280, "height": 900})
        pg = ctx.new_page()
        errs = []
        pageerrs = []
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
        pg.on("pageerror", lambda e: pageerrs.append(str(e)))
        saved = []
        posts = []

        def route_api(route):
            u = route.request.url; m = route.request.method
            if m == "GET" and u.endswith("/api/v1/story-tavern/packs"):
                return route.fulfill(status=200, content_type="application/json", body=json.dumps({"packs": [PACK]}))
            if m == "GET" and "/packs/demo" in u and "/chapters/" not in u:
                return route.fulfill(status=200, content_type="application/json", body=json.dumps(PACK))
            if m == "GET" and "/packs/demo/chapters/" in u:
                return route.fulfill(status=200, content_type="application/json", body=json.dumps({"content": "章节正文 mock", "path": u.split("chapters/")[-1]}))
            if m == "POST" and "/api/v1/story-tavern/packs" in u:
                try:
                    body = json.loads(route.request.post_data or "{}")
                    saved.append(body)
                except Exception:
                    pass
                return route.fulfill(status=200, content_type="application/json", body=json.dumps({"ok": True, "id": "demo"}))
            if "story-tavern/sessions" in u:
                return route.fulfill(status=200, content_type="application/json", body=json.dumps({"sessions": []}))
            return route.fallback()

        pg.route("**/story-tavern/**", route_api)
        pg.goto(BASE + "/web/", timeout=30000); pg.wait_for_timeout(500)
        pg.evaluate("(a)=>{for(const[k,v]of Object.entries(a))localStorage.setItem(k,v)}", ls)
        pg.reload(timeout=30000); pg.wait_for_timeout(800)

        # 档案馆
        pg.evaluate("()=>{const els=[...document.querySelectorAll('[data-tab=\"packs\"]')]; (els.find(e=>e.offsetParent!==null)||els[0]).click();}")
        pg.wait_for_timeout(1200)
        pg.evaluate("()=>{const els=[...document.querySelectorAll('#st-packs-listview .item')]; (els.find(e=>e.offsetParent!==null)||els[0]).click();}")
        pg.wait_for_timeout(1500)
        out["detail_title"] = pg.evaluate("()=>document.getElementById('st-pack-detail-title')?.textContent||''")

        # 点剧本图
        pg.evaluate("()=>{const b=document.getElementById('st-pack-graph'); b&&b.click();}")
        pg.wait_for_timeout(1800)
        out["overlay"] = pg.evaluate("()=>({vis: !!document.getElementById('stg-overlay'), title: document.getElementById('stg-title')?.textContent||''})")
        out["nodes"] = pg.evaluate("()=>document.querySelectorAll('#stg-overlay .stg-node').length")
        out["edges"] = pg.evaluate("()=>document.querySelectorAll('#stg-overlay path.stg-edge, #stg-overlay .stg-edges path').length")
        out["probe_canvas"] = pg.evaluate("()=>{const c=document.getElementById('stg-canvas'); return c? {exists:true, childCount:c.childElementCount, html:(c.innerHTML||'').slice(0,300)}:{exists:false}}")
        out["probe_state"] = pg.evaluate("()=>{const g=window.storyGraphEditor; const st=g&&g.getState(); return {hasState:!!st, packId:st&&st.packId, mode:st&&st.mode, nodeCount:st&&st.pack&&st.pack.nodes?st.pack.nodes.length:0, docKeys:st&&st.doc?Object.keys(st.doc):[]}}")
        out["node_titles"] = pg.evaluate("()=>[...document.querySelectorAll('#stg-overlay .stg-node .stg-node-title')].map(e=>e.textContent)")

        # 点第一个节点 → inspector
        pg.evaluate("()=>document.querySelector('#stg-overlay .stg-node')?.click()")
        pg.wait_for_timeout(500)
        out["inspector_has_title"] = pg.evaluate("()=>!!document.getElementById('stg-f-title')")
        out["inspector_entry"] = pg.evaluate("()=>document.getElementById('stg-f-entry')?.value||''")

        # 进入编辑 → 修改 entry → 保存
        pg.evaluate("()=>document.getElementById('stg-open-edit')?.click()")
        pg.wait_for_timeout(300)
        out["save_visible_edit"] = pg.evaluate("()=>{const s=document.getElementById('stg-save'); return s? !s.hidden : false}")
        pg.evaluate("()=>{const t=document.getElementById('stg-f-entry'); if(t){const setter=Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set; setter.call(t,'开局已修改'); t.dispatchEvent(new Event('input',{bubbles:true}));}}")
        pg.evaluate("()=>document.getElementById('stg-save')?.click()")
        pg.wait_for_timeout(800)
        out["save_has_nodes"] = bool(saved and saved[0].get("nodes"))
        out["save_entry_updated"] = bool(saved and saved[0]["nodes"] and saved[0]["nodes"][0]["entry"] == "开局已修改")
        out["save_pack_id"] = saved[0]["id"] if saved else None

        # 新增出口连线测试: 给 n3 加 exit→n1
        pg.evaluate("()=>{const p=window.storyGraphEditor&&storyGraphEditor.getState().pack; const n=p&&p.nodes.find(z=>z.id==='n3'); if(n){n.exit=n.exit||[]; n.exit.push({id:'e9x',when:'回去',next:'n1'});}}")
        pg.evaluate("()=>document.getElementById('stg-save')?.click()")
        pg.wait_for_timeout(800)
        n3 = next((z for z in (saved[-1].get("nodes") or []) if z["id"]=="n3"), None) if saved and saved[-1].get("nodes") else None
        out["n3_exits_after_add"] = len(n3["exit"]) if n3 else -1

        # 关闭
        pg.evaluate("()=>document.getElementById('stg-close')?.click()")
        pg.wait_for_timeout(300)
        out["closed"] = pg.evaluate("()=>!document.getElementById('stg-overlay')")
        out["pageerrors"] = pageerrs[:6]
        out["console_errors"] = errs[:10]
        out["ok"] = (out["nodes"] == 4 and out["edges"] == 3 and out["overlay"]["vis"] and out["inspector_has_title"] and out["save_has_nodes"] and out["save_entry_updated"])
        pg.screenshot(path="/tmp/p2_graph_e2e.png")
        ctx.close()
    print(json.dumps(out, ensure_ascii=False, indent=1))
    sys.exit(0 if out["ok"] else 2)

if __name__ == "__main__":
    main()
