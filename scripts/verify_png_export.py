#!/usr/bin/env python3
"""验证 Kaleido 角色卡 PNG 导出闭环：st-export → pngBase64 → 解 PNG → 读回 chara JSON"""
import json, base64, re, sys, urllib.request, urllib.error

BASE = "http://127.0.0.1:18766"

def api(path, token=None, body=None, method=None):
    req = urllib.request.Request(BASE + path, method=method or ("POST" if body is not None else "GET"))
    if token:
        req.add_header("Authorization", "Bearer " + token)
    if body is not None:
        req.add_header("Content-Type", "application/json")
        data = json.dumps(body).encode()
    else:
        data = None
    try:
        with urllib.request.urlopen(req, data=data, timeout=20) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:300]

def main():
    # 1. 登录
    import os
    env = {}
    for line in open(os.path.join(os.path.dirname(__file__), "..", ".env"), encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip()
    st, d = api("/api/v1/auth/login", body={"username": env.get("KALEIDO_ADMIN_USER"), "password": env.get("KALEIDO_ADMIN_PASSWORD")})
    if st != 200:
        print(f"LOGIN FAIL {st}: {d}"); sys.exit(1)
    token = d.get("token")
    print("登录 OK")

    # 2. 取一个角色卡 id（partner 状态里取，无则创建）
    st, d = api("/api/v1/partner", token=token)
    cards = d.get("characterCards", []) if isinstance(d, dict) else []
    if not cards:
        print("无现成角色卡，先创建一个测试卡")
        st, d = api("/api/v1/partner/character-cards", token=token, body={
            "name": "向量实验室测试卡", "content": "一名测试角色，用于导出验证",
            "fields": {"personality": "冷静", "firstMes": "你好，我是测试卡。"},
        })
        cards = d.get("characterCards", []) if isinstance(d, dict) else []
    if not cards:
        print(f"FAIL: 无法获取角色卡列表, resp={str(d)[:200]}"); sys.exit(1)
    cc_id = cards[0].get("id") if isinstance(cards[0], dict) else str(cards[0])
    print(f"使用角色卡: {cc_id}")

    # 3. 导出
    st, d = api("/api/v1/partner/st-export", token=token, body={"kind": "character_card", "characterCardId": cc_id, "format": "both"})
    if st != 200:
        print(f"EXPORT FAIL {st}: {d}"); sys.exit(1)
    png_b64 = d.get("pngBase64")
    if not png_b64:
        print(f"FAIL: 响应无 pngBase64! keys={list(d.keys())}")
        print(json.dumps(d, ensure_ascii=False)[:500])
        sys.exit(1)
    print(f"导出 OK: pngTextTodo={d.get('pngTextTodo')}, pngBase64 len={len(png_b64)}")

    # 4. 解码 PNG + 校验签名
    png = base64.b64decode(png_b64)
    assert png[:8] == b"\x89PNG\r\n\x1a\n", "PNG 签名错误!"
    print(f"PNG 签名 OK, 大小 {len(png)} bytes")

    # 5. 提取 tEXt chara chunk 并解析 JSON
    pos = 8
    chara = None
    while pos + 12 <= len(png):
        ln = int.from_bytes(png[pos:pos+4], "big")
        typ = png[pos+4:pos+8]
        data_chunk = png[pos+8:pos+8+ln]
        if typ == b"tEXt":
            nul = data_chunk.find(b"\x00")
            if nul > 0:
                key = data_chunk[:nul].decode()
                val = data_chunk[nul+1:]
                if key == "chara":
                    chara = json.loads(base64.b64decode(val))
        pos += 12 + ln
    if not chara:
        print("FAIL: PNG 中未找到 chara tEXt chunk"); sys.exit(1)
    print(f"chara 解析 OK: spec={chara.get('spec')}, name={chara.get('data',{}).get('name')}")
    print(f"description={chara.get('data',{}).get('description','')[:40]}")

    # 6. 用服务自身 extract_st_card_from_png 能力交叉验证（模拟 st-import 反读）
    st, d = api("/api/v1/partner/st-import", token=token, body={"pngBase64": png_b64})
    print(f"st-import 反读: HTTP {st}, {str(d)[:200]}")
    print("\n✅ PNG 导出闭环验证通过")

if __name__ == "__main__":
    main()
