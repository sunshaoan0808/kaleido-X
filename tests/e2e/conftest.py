"""剧场 E2E conftest
驱动:复用 9222 CDP chrome(有头真实渲染,生产服务器),独立 Pixel5 context(安卓 UA+mobile UI)
登录:注入 fixtures/ls_real.json 完整 localStorage 快照(真实用户态),appearance 强制 mode + syncServer:false(避免服务器主题覆盖)
动态 token:若设置 KALEIDO_TEST_ADMIN_USER/PASSWORD,每个页面创建前 POST /api/v1/auth/login 换新 token
    (防共享环境下 max_sessions auto_evict 踢掉快照 token);无凭据则 fallback 快照 token
坑:并发环境(多测试进程并行 login admin)会触发 max_sessions auto_evict,踢掉快照 token
"""
import json, os, urllib.request
import pytest
from playwright.sync_api import sync_playwright

BASE = "http://127.0.0.1:18766"
FULL_SID = os.environ.get("KALEIDO_TEST_SID", "tavern-session-00000000-0000-0000-0000-000000000000")
SESSION_URL = f"{BASE}/web/#/tavern/session/{FULL_SID}"
CDP_ENDPOINT = "http://127.0.0.1:9222"
CHROME = "/usr/bin/google-chrome-stable"
HERE = os.path.dirname(os.path.abspath(__file__))
LS_FIXTURE = os.path.join(HERE, "fixtures", "ls_real.json")

def refresh_token():
    """动态登录换新 token;无凭据/失败返回 None(调用方 fallback 快照)"""
    user = os.environ.get("KALEIDO_TEST_ADMIN_USER", "admin")
    pw = os.environ.get("KALEIDO_TEST_ADMIN_PASSWORD")
    if not pw:
        return None
    try:
        req = urllib.request.Request(
            BASE + "/api/v1/auth/login",
            data=json.dumps({"username": user, "password": pw}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST")
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = json.loads(resp.read())
        tok = body.get("token") if isinstance(body, dict) else None
        if tok:
            print(f"[conftest] dynamic token refreshed ({tok[:8]}...)")
        return tok
    except Exception as e:
        print(f"[conftest] dynamic login failed ({e}), fallback snapshot token")
        return None

def load_ls(mode="day"):
    """加载真实 localStorage 快照,appearance 强制 mode + 关闭服务器同步;有动态 token 则替换"""
    ls = json.load(open(LS_FIXTURE, encoding="utf-8"))
    tok = refresh_token()
    if tok:
        ls["kaleido_token"] = tok
    try:
        app = json.loads(ls["kaleido_appearance_v1"])
    except Exception:
        app = {}
    app["mode"] = mode
    app["syncServer"] = False
    ls["kaleido_appearance_v1"] = json.dumps(app, ensure_ascii=False)
    return ls

def make_page(pw_or_browser, browser, mode="day", wait_ms=9000):
    """创建独立 Pixel5 context,注入登录态,进入剧场会话;返回 page(已加载)"""
    try:
        ctx = browser.new_context(**pw_or_browser.devices["Pixel 5"])
    except Exception:
        ctx = browser.new_context(viewport={"width": 393, "height": 844})
    pg = ctx.new_page()
    pg.goto(BASE + "/web/", timeout=30000)
    pg.wait_for_timeout(300)
    pg.evaluate("(a)=>{for(const[k,v]of Object.entries(a))localStorage.setItem(k,v)}", load_ls(mode))
    pg.reload(timeout=30000)  # SPA 首次初始化后才注入 LS,必须 reload 才会读取注入的登录态/主题
    pg.wait_for_timeout(500)
    pg.goto(SESSION_URL, timeout=30000)
    pg.wait_for_timeout(wait_ms)
    return pg

@pytest.fixture(scope="session")
def pw():
    with sync_playwright() as p:
        yield p

@pytest.fixture(scope="session")
def browser(pw):
    try:
        b = pw.chromium.connect_over_cdp(CDP_ENDPOINT)
        print("driver: CDP", CDP_ENDPOINT)
    except Exception as e:
        print("WARN: CDP unavailable, fallback headless Pixel5:", e)
        b = pw.chromium.launch(headless=True, executable_path=CHROME)
    yield b

@pytest.fixture()
def page(pw, browser):
    pg = make_page(pw, browser, "day")
    yield pg
    pg.context.close()

@pytest.fixture()
def page_night(pw, browser):
    pg = make_page(pw, browser, "night")
    yield pg
    pg.context.close()
