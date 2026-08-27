#!/usr/bin/env python3
"""X6/X7 全流程验收：构造四种格式角色卡走 /st-import 端点实证。"""
import base64, json, struct, sys, urllib.request

BASE = "http://127.0.0.1:18766"
SESS = "/api/v1/partner/st-import"
TOKEN = None

def login():
    global TOKEN
    req = urllib.request.Request(
        BASE + "/api/v1/auth/login",
        data=json.dumps({"username": "admin", "password": "<KALEIDO_PASS>"}).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=10) as r:
        d = json.loads(r.read())
        TOKEN = d.get("token")
    if not TOKEN:
        print("!! 登录失败")

def post(path, data, ct):
    h = {"Content-Type": ct}
    if TOKEN:
        h["Authorization"] = "Bearer " + TOKEN
    req = urllib.request.Request(BASE + path, data=data, method="POST", headers=h)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:200]

# --- 构造 V3 卡片 JSON（base64 用） ---
CARD = {
    "spec": "chara_card_v3",
    "spec_version": "3.0",
    "data": {
        "name": "验收测试姬",
        "description": "全流程验收用角色卡",
        "personality": "严谨",
        "first_mes": "*翻开验收清单* 开始吧。",
        "scenario": "验收现场",
        "world_book": {"entries": [
            {"keys": ["验收"], "content": "验收要点：必须全流程实证。", "enabled": True},
            {"keys": ["禁用"], "content": "这条不该出现。", "enabled": False},
        ]},
    },
}
CARD_B64 = base64.b64encode(json.dumps(CARD).encode()).decode()

# --- 1. V1 平铺 JSON ---
def v1_json():
    v1 = {"char_name": "V1验收", "char_persona": "平铺时代", "world_scenario": "旧酒馆",
          "char_greeting": "*旧式开场* 你好。", "example_dialogue": "{{char}}: 嗯。"}
    return json.dumps(v1).encode()

# --- 2. PNG (tEXt chara) ---
def png_card():
    import zlib
    def chunk(typ, data):
        return struct.pack(">I", len(data)) + typ + data + struct.pack(">I", zlib.crc32(typ + data) & 0xffffffff)
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
    text = b"chara\x00" + CARD_B64.encode()
    return sig + chunk(b"IHDR", ihdr) + chunk(b"tEXt", text) + chunk(b"IEND", b"")

# --- 3. WEBP (XMP chunk 带 ccv3) ---
def webp_card():
    xmp = (b'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
           b'<rdf:Description rdf:about="" xmlns:ccv3="http://localhost/ccv3">'
           b'<ccv3:chara_card_v3>' + CARD_B64.encode() + b'</ccv3:chara_card_v3>'
           b'</rdf:Description></rdf:RDF></x:xmpmeta>')
    def chunk(typ, data):
        pad = b"\0" if len(data) % 2 else b""
        return typ + struct.pack("<I", len(data)) + data + pad
    body = chunk(b"XMP ", xmp)
    return b"RIFF" + struct.pack("<I", 4 + len(body)) + b"WEBP" + body

# --- 4. JPEG (APP1 EXIF UserComment) ---
def jpeg_card():
    payload = b"ASCII\0\0\0" + CARD_B64.encode()
    tiff = b"II\x2a\0" + struct.pack("<I", 8) + struct.pack("<H", 1)
    tiff += struct.pack("<H", 0x9286) + struct.pack("<H", 7) + struct.pack("<I", len(payload))
    tiff += struct.pack("<I", 26) + struct.pack("<I", 0) + payload
    seg = b"Exif\0\0" + tiff
    return b"\xFF\xD8" + b"\xFF\xE1" + struct.pack(">H", len(seg) + 2) + seg + b"\xFF\xD9"

def main():
    login()
    if not TOKEN:
        sys.exit(2)
    results = []
    cases = [
        ("V1 JSON", v1_json(), "application/json"),
        ("PNG tEXt", png_card(), "image/png"),
        ("WEBP XMP", webp_card(), "image/webp"),
        ("JPEG EXIF", jpeg_card(), "image/jpeg"),
    ]
    for name, data, ct in cases:
        st, resp = post(SESS, data, ct)
        ok = st == 200 and isinstance(resp, dict) and resp.get("ok")
        source = resp.get("source", "?") if isinstance(resp, dict) else "?"
        results.append((name, st, ok, source))
        print(f"{name:12s} -> HTTP {st} ok={ok} source={source}")

    allok = all(r[2] for r in results)
    print(f"\n{'='*50}\n验收结果: {'✅ 4/4 全过' if allok else '❌ 有失败'}")
    sys.exit(0 if allok else 1)

if __name__ == "__main__":
    main()
