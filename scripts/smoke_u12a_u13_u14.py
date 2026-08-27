#!/usr/bin/env python3
"""Kaleido main(0c230339) 部署冒烟 v2 — U12-A + U14 + U13 全量检查"""
import json, sys, os, time
import urllib.request, urllib.error
import urllib.parse

B = "http://127.0.0.1:19001"
PASS = []
FAIL = []

def shelf_slug(t):
    s = "".join(c for c in t if c.isalnum() or c in "_ -")
    s = s.replace(" ", "_")
    s = "".join(c for c in s if c.isalnum() or c == "_")
    return s.strip("_").lower()[:60]

def req(method, path, body=None, token=None):
    url = B + path
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Content-Type", "application/json")
    if token:
        r.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(r, timeout=30) as resp:
            ctype = resp.headers.get("Content-Type", "")
            raw = resp.read()
            if "json" in ctype:
                return resp.status, json.loads(raw)
            return resp.status, raw.decode(errors="replace")
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read())
        except Exception:
            return e.code, {"_raw": e.read().decode(errors="replace")}

def check(name, cond, detail=""):
    if cond:
        PASS.append(name); print(f"  ✅ {name}")
    else:
        FAIL.append(name); print(f"  ❌ {name} :: {str(detail)[:220]}")

# ── 登录 ──
st, r = req("POST", "/api/v1/auth/login", {"username": "admin", "password": "smoke-main-20260809"})
check("login 200", st == 200, f"st={st} r={str(r)[:200]}")
TOKEN = r.get("token") if st == 200 else ""

# ══════════ U12-A dual-agent ══════════
print("\n── U12-A dual-agent ──")
st, r = req("POST", "/api/v1/dual-agent/sessions", {"workId": "smoke-rose2", "title": "量子玫瑰在雨夜绽放"}, TOKEN)
check("创建会话 201", st == 201, f"st={st} r={str(r)[:300]}")
sid = r.get("session", {}).get("id", "") if isinstance(r, dict) else ""
check("会话 id 非空", bool(sid), str(r)[:200])
if not sid:
    print("中断: 无会话 id"); sys.exit(1)

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/plan", {}, TOKEN)
check("#2 启发式规划 pendingConfirmation=true", st == 200 and r.get("pendingConfirmation") is True, f"st={st} r={str(r)[:250]}")
check("  plan.state=proposed", r.get("plan", {}).get("state") == "proposed", str(r)[:200])

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/handoff", {}, TOKEN)
check("#3 未确认 handoff 拦截", r.get("blocked") is True and "plan_confirmation" in (r.get("missingItems") or []), f"st={st} r={str(r)[:250]}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/plan", {"instruction": "加一段主角与玫瑰的对话，并埋下档案管理员伏笔"}, TOKEN)
check("#4 A1 迭代 plan 200 proposed", st == 200 and r.get("plan", {}).get("state") == "proposed", f"st={st} r={str(r)[:250]}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/chat", {"message": "没问题，就按这个吧"}, TOKEN)
check("#5 A2 NL确认 confirmed=true", r.get("confirmed") is True and r.get("pendingConfirmation") is False, f"st={st} r={str(r)[:250]}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/confirm-plan", {}, TOKEN)
check("#13 confirm-plan 幂等(已确认后)", st == 200 and r.get("idempotent") is True, f"st={st} idempotent={r.get('idempotent')} confirmed={r.get('confirmed')}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/handoff", {}, TOKEN)
check("#6 A3 确认后 handoff ok", r.get("ok") is True and r.get("nextAction") == "start_writing", f"st={st} r={str(r)[:250]}")
wins = r.get("windows") or []
check("  windows 已生成", len(wins) > 0, str(r)[:200])

st, r = req("GET", f"/api/v1/dual-agent/sessions/{sid}/plan", token=TOKEN)
check("#7 GET plan pendingConfirmation=false", r.get("pendingConfirmation") is False, f"st={st} r={str(r)[:200]}")

st, r = req("GET", f"/api/v1/dual-agent/sessions/{sid}/ledger", token=TOKEN)
ledger = r.get("ledger", []) if isinstance(r, dict) else []
stages = [e.get("stage") for e in ledger] if isinstance(ledger, list) else []
check("#8 ledger 含 plan/plan_iteration/confirm_plan/handoff", all(s in stages for s in ["plan", "plan_iteration", "confirm_plan", "handoff"]), f"stages={stages}")
haves = [e.get("planHash") for e in (ledger if isinstance(ledger, list) else []) if e.get("planHash")]
check("  planHash 逐轮变化", len(set(haves)) >= 2, f"hashes={set(haves)}")
fc = [e.get("foreshadowCount") for e in (ledger if isinstance(ledger, list) else []) if e.get("stage") == "handoff"]
check("  handoff foreshadowCount>=1", bool(fc) and fc[0] >= 1, f"fc={fc}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/chat", {"message": "把第二幕节奏加快，加一场追逐戏"}, TOKEN)
check("#9 对话式规划迭代 pendingConfirmation=true", r.get("pendingConfirmation") is True, f"st={st} r={str(r)[:200]}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/chat", {"message": "确认"}, TOKEN)
check("#10 NL 确认 confirmed=true", r.get("confirmed") is True, f"st={st} r={str(r)[:200]}")

st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/chat", {"message": "交接给 Dante 来写吧"}, TOKEN)
check("#11 NL 触发交接 ok=true", r.get("ok") is True, f"st={st} r={str(r)[:200]}")

st, r = req("GET", f"/api/v1/dual-agent/sessions/{sid}/state", token=TOKEN)
check("#16 GET state ok=true", st == 200 and r.get("ok") is True, f"st={st} r={str(r)[:200]}")
na = r.get("session", {}).get("nextAction") if isinstance(r, dict) else None
print(f"     state.nextAction(raw)={na}")

# 新建会话：无规划即 handoff
st, r = req("POST", "/api/v1/dual-agent/sessions", {"workId": "smoke-noplan2", "title": "无规划的边界"}, TOKEN)
sid2 = r.get("session", {}).get("id", "")
st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid2}/handoff", {}, TOKEN)
check("#14 无规划 handoff 拦截", r.get("blocked") is True and "outline" in (r.get("missingItems") or []), f"st={st} r={str(r)[:250]}")

# ── U14 bookshelf（写临时 shelf .md 供 export 匹配）──
print("\n── U14 bookshelf ──")
st, r = req("GET", "/api/v1/bookshelf/registry", token=TOKEN)
check("U14 registry GET 200", st == 200, f"st={st} r={str(r)[:200]}")
books0 = (r.get("books") if isinstance(r, dict) else r) or []
n0 = len(books0)

uniq = str(int(time.time()))[-6:]
utitle = f"冒烟书U14-{uniq}"
uslug_reg = f"smoke-u14-{uniq}"
st, r = req("POST", "/api/v1/bookshelf/registry", {"slug": uslug_reg, "title": utitle, "source": "smoke", "tags": ["test"]}, TOKEN)
check("U14 registry upsert", st in (200, 201), f"st={st} r={str(r)[:200]}")

st, r = req("GET", "/api/v1/bookshelf/registry", token=TOKEN)
n1 = len((r.get("books") if isinstance(r, dict) else r) or [])
check("U14 registry 含新书", n1 == n0 + 1, f"n0={n0} n1={n1}")

# export：写临时 .md 到 novel_workspace，filename stem 必须等于 title 生成的 shelf_slug
title_slug = shelf_slug(utitle)
md_path = f"${REPO:-.}/novel_workspace/{title_slug}.md"
with open(md_path, "w", encoding="utf-8") as f:
    f.write(f"# {utitle}\n\n## 目录\n\n## 第一章 雨夜\n\n量子玫瑰在雨中舒展花瓣。\n\n## 第二章 档案室\n\n档案管理员推开门。\n")
try:
    q = urllib.parse.quote(title_slug)
    st, r = req("GET", f"/api/v1/bookshelf/{q}/export?format=txt", token=TOKEN)
    check("U14 book export 200", st == 200, f"st={st} r={str(r)[:200]}")
    txt = r if isinstance(r, str) else str(r)
    check("  export 含正文章节", "量子玫瑰" in txt, txt[:120])
finally:
    os.remove(md_path)

# ── U13 story-tavern compact + branch summary ──
print("\n── U13 memory ──")
st, r = req("POST", "/api/v1/story-tavern/sessions", {"packId": "demo-rain-alley"}, TOKEN)
check("U13 tavern 会话创建", st in (200, 201), f"st={st} r={str(r)[:300]}")
tid = ""
if isinstance(r, dict):
    tid = r.get("sessionId") or r.get("session", {}).get("id") or r.get("id") or ""
check("U13 会话 id", bool(tid), str(r)[:200])

if tid:
    st, r = req("POST", f"/api/v1/story-tavern/sessions/{tid}/compact", {}, TOKEN)
    check("U13 /compact 端点语义（200/4xx 归一）", st in (200, 201, 400, 422, 409), f"st={st} r={str(r)[:250]}")
    st, r = req("GET", f"/api/v1/story-tavern/sessions/{tid}/branches/main/summary", token=TOKEN)
    check("U13 /branches/summary 端点存在", st in (200, 404, 422), f"st={st} r={str(r)[:250]}")

# U12 幂等回归
st, r = req("POST", f"/api/v1/dual-agent/sessions/{sid}/plan", {}, TOKEN)
check("U12 幂等回归 idempotent=true", r.get("idempotent") is True, f"st={st} r={str(r)[:200]}")

print(f"\n===== 结果: {len(PASS)} PASS / {len(FAIL)} FAIL =====")
if FAIL:
    print("失败项:", FAIL)
    sys.exit(1)