#!/usr/bin/env python3
"""S7 历史向量回忆端到端验证：
1. 触发一次 turn（温床会话超阈值 → weave 压缩 → S7 archive 写入向量索引）
2. 检查 sess-{id}.json 索引文件生成且 entries>0
3. 再触发一次 turn（同 query → S7 recall 命中注入日志）
"""
import json, os, sys, time, urllib.request, urllib.error, glob

BASE = "http://127.0.0.1:18766"
SID = "tavern-session-3b107d60-2afb-48a9-844a-6e56d4b80018"  # 温床 turn=118

def api(path, token=None, body=None, timeout=60):
    req = urllib.request.Request(BASE + path, method="POST" if body is not None else "GET")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    if body is not None:
        req.add_header("Content-Type", "application/json")
        data = json.dumps(body).encode()
    else:
        data = None
    try:
        with urllib.request.urlopen(req, data=data, timeout=timeout) as r:
            raw = r.read().decode()
            try:
                return r.status, json.loads(raw)
            except Exception:
                return r.status, raw[:500]
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:300]
    except Exception as e:
        return -1, f"NET: {e}"

def main():
    env = {}
    for line in open(os.path.join(os.path.dirname(__file__), "..", ".env"), encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip()
    st, d = api("/api/v1/auth/login", body={"username": env.get("KALEIDO_ADMIN_USER"), "password": env.get("KALEIDO_ADMIN_PASSWORD")})
    if st != 200:
        print(f"LOGIN FAIL {st}"); sys.exit(1)
    token = d.get("token")
    print("登录 OK")

    idx_dir = os.path.join(os.path.dirname(__file__), "..", "data", "state", "wi-vector-index")
    sess_idx = os.path.join(idx_dir, f"sess-{SID}.json")
    if os.path.exists(sess_idx):
        print(f"清理旧索引: {sess_idx}")
        os.remove(sess_idx)

    # 1. 第一次 turn：触发 weave + archive
    print(f"\n=== 第1次 turn（触发 weave → S7 archive）===")
    st, d = api(f"/api/v1/story-tavern/sessions/{SID}/turn", token=token,
                body={"message": "（S7 验证）继续推进剧情，简短回应即可。", "kind": "story"},
                timeout=180)
    print(f"turn1 HTTP {st}")
    if st == -1:
        print(f"turn1 网络异常: {d}")
    time.sleep(2)

    # 2. 检查索引文件
    print(f"\n=== 检查向量索引 ===")
    if os.path.exists(sess_idx):
        idx = json.load(open(sess_idx))
        print(f"✅ sess 索引生成: entries={len(idx.get('entries', []))}, dim={idx.get('dim')}, model={idx.get('model')}")
        if idx.get('entries'):
            print(f"   首条: {json.dumps(idx['entries'][0], ensure_ascii=False)[:150]}")
    else:
        print(f"❌ 索引未生成 (期望 {sess_idx})")
        # 列出已有 sess-* 文件帮助诊断
        existing = glob.glob(os.path.join(idx_dir, "sess-*"))
        print(f"   现有 sess-* 文件: {existing}")

    # 3. 第二次 turn：验证 recall 注入
    print(f"\n=== 第2次 turn（验证 S7 recall）===")
    st, d = api(f"/api/v1/story-tavern/sessions/{SID}/turn", token=token,
                body={"message": "（S7 验证）刚才发生的事你还记得吗？简述一下。", "kind": "story"},
                timeout=180)
    print(f"turn2 HTTP {st}")
    print("\n完成。查看服务日志确认 'S7 history archived' / 'S7 history recall injected'")

if __name__ == "__main__":
    main()
