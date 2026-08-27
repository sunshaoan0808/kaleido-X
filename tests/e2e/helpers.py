"""剧场 UI 自动化回归 helpers:主题断言/重叠检测/溢出检测"""
import json

def ev(pg, js, *args):
    return pg.evaluate(js, *args)

def info(pg, selector):
    """元素几何+样式快照;不存在返回 None"""
    return ev(pg, """(sel)=>{const e=document.querySelector(sel);if(!e)return null;
      const r=e.getBoundingClientRect(), cs=getComputedStyle(e);
      const vw=window.innerWidth, vh=window.innerHeight;
      return {tag:e.tagName, cls:e.className, id:e.id,
        t:Math.round(r.top), b:Math.round(r.bottom), l:Math.round(r.left), rr:Math.round(r.right),
        w:Math.round(r.width), h:Math.round(r.height),
        display:cs.display, visibility:cs.visibility, opacity:cs.opacity,
        z:cs.zIndex, pos:cs.position,
        bg:cs.backgroundColor, color:cs.color,
        font:cs.fontFamily,
        inView: r.top>=0&&r.left>=0&&r.bottom<=vh+1&&r.right<=vw+1,
        scrolledX: e.scrollWidth>e.clientWidth, scrolledY: e.scrollHeight>e.clientHeight,
        scrollW:e.scrollWidth, clientW:e.clientWidth,
        scrollH:e.scrollHeight, clientH:e.clientHeight,
        overflowX:cs.overflowX, overflowY:cs.overflowY,
        visible: cs.display!=='none'&&cs.visibility!=='hidden'&&+cs.opacity>0
      }}""", selector)

def center_point(sel):
    return f"""(()=>{{const e=document.querySelector({json.dumps(sel)});if(!e)return null;
      const r=e.getBoundingClientRect();return {{x:Math.round(r.left+r.width/2),y:Math.round(r.top+r.height/2),w:Math.round(r.width),h:Math.round(r.height)}};}})()"""

def hit_owner(pg, x, y):
    """elementFromPoint 命中的元素链(含父级 id/class),判断是否属于某容器"""
    return ev(pg, """(p)=>{const el=document.elementFromPoint(p.x,p.y);
      if(!el)return null;const chain=[];let n=el;
      for(let i=0;i<6&&n;i++,n=n.parentElement){
        chain.push({tag:n.tagName,id:n.id,cls:(typeof n.className==='string'?n.className.slice(0,60):'')});
      }return chain;}""", {"x": x, "y": y})

def overlap(pg, sel, container_hint=""):
    """检测 sel 中心点是否被非自身后代元素覆盖;返回 (被覆盖?, 命中链或'not-rendered')"""
    p = ev(pg, center_point(sel))
    if not p or p["w"] <= 0 or p["h"] <= 0:
        return None, "not-rendered"
    chain = hit_owner(pg, p["x"], p["y"])
    # elementFromPoint 命中链中包含目标元素本身或其容器 => 未被覆盖
    covered = ev(pg, f"""(function(){{const el=document.querySelector({json.dumps(sel)});
      if(!el)return false;const top=document.elementFromPoint({p['x']},{p['y']});
      let n=top;while(n&&n!==document.body){{if(n===el)return false;n=n.parentElement}};return true}})()""")
    return covered, chain

def theme_var(pg, var):
    return ev(pg, f"getComputedStyle(document.documentElement).getPropertyValue({json.dumps(var)}).trim()")

def body_bg(pg):
    return ev(pg, "getComputedStyle(document.body).backgroundColor")

def rel_lum(hexcolor):
    """相对亮度 0-1,用于日/夜背景判断"""
    import re
    m = re.match(r"rgba?\((\d+),\s*(\d+),\s*(\d+)", hexcolor)
    if not m:
        return None
    r, g, b = (int(v) / 255 for v in m.groups())
    for i, c in enumerate((r, g, b)):
        c = c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4
        r, g, b = (r, g, b)
        if i == 0: rr = c
        elif i == 1: gg = c
        else: bb = c
    return 0.2126 * rr + 0.7152 * gg + 0.0722 * bb
