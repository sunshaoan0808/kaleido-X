#!/usr/bin/env python3
"""为度蜜月 pack 的 16 张事件卡补 chapterRange 测试数据（模拟 A1 蒸馏产物）。

背景（2026-08-18）：事件卡蒸馏此前不产出 chapterRange，旧 pack 全无该字段。
本脚本基于已掌握的剧情章节知识给每张卡标注 chXX 范围，用于验证 A2 按章过滤的
运行时行为（A3 兼容路径下旧包无标注行为不变；本注入让 16 张卡全部带标注，
从而能实测「ch01 时全被过滤、后期章节正常浮现」）。

⚠️ 这是验证 A2 的测试数据，不是生产蒸馏产物。新 pack 的 chapterRange 由蒸馏 LLM
自动产出（A1）。本脚本可重复运行（幂等）。

剧情章节知识（原著 17 章，ch01 学校序幕无事件卡——这正是首回合劫持的根源）：
- 前期 (ch03-ch06):  到达三亚、月见屋入住、蜜月第一晚、SPA
- 中期 (ch07-ch12):  验孕棒、沈雨棠约会线、相亲、公交站、综艺
- 后期 (ch13-ch16):  产房、国栋视频、门锁、新生儿、婚后家庭日常
"""
import json
import sys

PACK = sys.argv[1] if len(sys.argv) > 1 else \
    "data/story-packs/pack-shelf-代替父亲和妈妈度蜜月-2007"

# card-id → chapterRange（依据剧情章节知识，见文档 §9/§10）
RANGES = {
    # pkg-adventure
    "card-1-1": ["ch03", "ch05"],   # 月见屋的蜜月之夜（到达+蜜月第一晚）
    "card-1-2": ["ch04", "ch06"],   # SPA后的失控
    "card-1-3": ["ch08", "ch10"],   # 验孕棒的两条杠
    "card-1-4": ["ch07", "ch09"],   # 主卧的狂乱
    # pkg-daily
    "card-2-1": ["ch14", "ch16"],   # 父亲的鲫鱼汤晚餐（婚后回门/家庭）
    "card-2-2": ["ch14", "ch16"],   # 念念的毛绒兔子（新生儿）
    "card-2-3": ["ch13", "ch15"],   # 阳台的绿萝与新闻联播（孕后期居家）
    "card-2-4": ["ch15", "ch16"],   # 婴儿车里的海（出生后）
    # pkg-romance
    "card-3-1": ["ch08", "ch10"],   # 沈雨棠的成长手册
    "card-3-2": ["ch10", "ch12"],   # 公交站的路灯告别
    "card-3-3": ["ch09", "ch11"],   # 相亲饭局的三喜临门
    "card-3-4": ["ch08", "ch09"],   # 综艺节目下的橘子
    # pkg-crisis
    "card-4-1": ["ch05", "ch07"],   # 国栋的视频电话
    "card-4-2": ["ch12", "ch13"],   # 产房外的等待
    "card-4-3": ["ch11", "ch12"],   # 主卧门锁的咔哒
    "card-4-4": ["ch03", "ch04"],   # 度假村的木牌（到达时）
}

path = f"{PACK}/pack.json"
with open(path, encoding="utf-8") as f:
    pack = json.load(f)

updated = 0
missing = []
for ep in pack.get("eventPackages", []):
    for card in ep.get("cards", []):
        cid = card.get("id", "")
        if cid in RANGES:
            card["chapterRange"] = RANGES[cid]
            updated += 1
        else:
            missing.append(cid)

with open(path, "w", encoding="utf-8") as f:
    json.dump(pack, f, ensure_ascii=False, indent=2)

print(f"OK: {updated}/{updated + len(missing)} 张卡已标注 chapterRange")
if missing:
    print(f"未匹配: {missing}")