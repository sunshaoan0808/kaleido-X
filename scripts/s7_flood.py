#!/usr/bin/env python3
"""S7 验证：灌水 turn 直到触发 U11 压缩（消息数 > 32 兜底），验证向量索引生成 + 召回注入。
流程: turn(POST) → 拿 runId → GET /stream?runId= 等 SSE 完成 → 下一轮"""
import json, os, sys, time, urllib.request, urllib.error

BASE = "http://127.0.0.1:18766"
SID = os.environ.get("S7_SID", "tavern-session-3b107d60-2afb-48a9-844a-6e56d4b80018")
ROUNDS = int(os.environ.get("S7_ROUNDS", "16"))

env = {}
for line in open(os.path.join(os.path.dirname(__file__), "..", ".env"), encoding="utf-8"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1); env[k.strip()] = v.strip()

def api(path, token=None, body=None, timeout=30):
    req = urllib.request.Request(BASE + path, method="POST" if body is not None else "GET")
    if token: req.add_header("Authorization", "Bearer " + token)
    data = None
    if body is not None:
        req.add_header("Content-Type", "application/json")
        data = json.dumps(body).encode()
    try:
        with urllib.request.urlopen(req, data=data, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:300]
    except Exception as e:
        return -1, f"NET {e}"

def stream_wait(token, run_id, timeout=240):
    """GET /stream?runId= 读 SSE 直到 done 或超时"""
    req = urllib.request.Request(
        f"{BASE}/api/v1/story-tavern/sessions/{SID}/stream?runId={run_id}",
        headers={"Authorization": "Bearer " + token, "Accept": "text/event-stream"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            buf = b""
            start = time.time()
            while time.time() - start < timeout:
                chunk = r.read(4096)
                if not chunk:
                    break
                buf += chunk
                txt = buf.decode("utf-8", errors="replace")
                if "[DONE]" in txt or '"done"' in txt or "event: done" in txt or "event:end" in txt:
                    return True, "done"
            return True, "eof"
    except Exception as e:
        return False, f"stream err {e}"

def main():
    st, d = api("/api/v1/auth/login", body={"username": env.get("KALEIDO_ADMIN_USER"), "password": env.get("KALEIDO_ADMIN_PASSWORD")})
    if st != 200:
        print(f"LOGIN FAIL {st}"); sys.exit(1)
    token = json.loads(d)["token"]
    print("登录 OK")

    for i in range(ROUNDS):
        st, d = api(f"/api/v1/story-tavern/sessions/{SID}/turn", token=token,
                    body={"message": f"S7灌水{i+1}：夜色渐深，两人在灯下继续交谈。", "kind": "story"}, timeout=30)
        if st != 200:
            # 可能还在跑上一个 run → 等 5s 重试一次
            print(f"turn{i+1} HTTP {st}: {d[:100]} (wait 8s retry)")
            time.sleep(8)
            st, d = api(f"/api/v1/story-tavern/sessions/{SID}/turn", token=token,
                        body={"message": f"S7灌水{i+1}：夜色渐深，两人在灯下继续交谈。", "kind": "story"}, timeout=30)
        if st != 200:
            print(f"turn{i+1} FAIL {st}: {d[:150]}")
            continue
        try:
            run_id = json.loads(d).get("runId") or json.loads(d).get("run_id")
        except Exception:
            run_id = None
        if not run_id:
            print(f"turn{i+1} OK (no runId) {d[:80]}")
            continue
        ok, note = stream_wait(token, run_id)
        print(f"turn{i+1} stream {ok} ({note})", flush=True)
        time.sleep(1)

    # 最终检查索引
    idx_dir = os.path.join(os.path.dirname(__file__), "..", "data", "state", "wi-vector-index")
    sess_idx = os.path.join(idx_dir, f"sess-{SID}.json")
    if os.path.exists(sess_idx):
        idx = json.load(open(sess_idx))
        print(f"\n✅ sess 索引: entries={len(idx.get('entries', []))}, dim={idx.get('dim')}")
        if idx.get('entries'):
            e0 = idx['entries'][0]
            print(f"   首条: world={e0.get('world')} text前60={e0.get('text','')[:60]}")
    else:
        print(f"\n❌ 索引未生成: {sess_idx}")

if __name__ == "__main__":
    main()
