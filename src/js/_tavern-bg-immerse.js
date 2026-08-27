/**
 * _tavern-bg-immerse.js — 角色背景沉浸模式（对标 Agnai 沉浸模式）
 *
 * 剧场聊天区背景随当前发言角色动态切换：复用 P2-1 立绘管线
 * （stSpriteOf 解析当前发言角色的情绪立绘/avatar），将其作为
 * #st-view-play 的背景层（半透明 + 模糊 + 渐变遮罩），保证文字可读。
 *
 * 触发时机：stRenderSprite 每次被调用时同步刷新（消息流式更新、
 * 重渲染、会话切换均覆盖）；资料抽屉打开时也刷新。
 * 无角色图 / 无发言者时移除角色背景，回退全局壁纸（#app-wallpaper）。
 */

(function () {
  'use strict';

  /** 生成角色背景 URL——复用立绘管线，但背景图不随情绪变化（保持稳定） */
  function stImmerseBgUrl() {
    const msgs = (tavernSession && tavernSession.messages) || [];
    // 回溯最近一条有发言人前缀的 assistant 消息
    let name = null;
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      if (m && m.role && m.role !== 'user') {
        const s = stSpeakerNameOf(m);
        if (s) { name = s; break; }
      }
    }
    const pack = tavernPack;
    if (!pack || !Array.isArray(pack.characters)) return null;
    // 有明确发言者：优先其 avatar
    if (name) {
      const cid = stCharIdOf(name);
      const ch = pack.characters.find(function (c) { return c && String(c.id) === String(cid); });
      if (ch) {
        const av = (ch.avatar && String(ch.avatar).trim()) ? String(ch.avatar) : null;
        if (av) return av;
        try {
          const u = stSpriteOf(name);
          if (u) return u;
        } catch (_) {}
      }
    }
    // 叙述体/无发言者：fallback 到在场角色中第一个有 avatar 的（保持背景稳定）
    // 优先 presentCharacterIds，其次 cast 顺序
    const present = (tavernSession && tavernSession.presentCharacterIds) || [];
    const ordered = (present.length ? present : []).concat(pack.characters.map(function (c) { return c.id; }));
    for (let i = 0; i < ordered.length; i++) {
      const cid2 = String(ordered[i]);
      const ch2 = pack.characters.find(function (c) { return c && String(c.id) === cid2; });
      if (ch2 && ch2.avatar && String(ch2.avatar).trim()) return String(ch2.avatar);
    }
    // 最后兜底：cast 顺序第一个有 avatar 的角色
    for (let j = 0; j < pack.characters.length; j++) {
      const c3 = pack.characters[j];
      if (c3 && c3.avatar && String(c3.avatar).trim()) return String(c3.avatar);
    }
    return null;
  }

  /** 刷新剧场角色背景。null URL → 移除角色背景（回退全局壁纸） */
  function stRefreshImmerseBg() {
    const stage = document.getElementById('st-view-play');
    if (!stage) return;
    try {
      const url = stImmerseBgUrl();
      if (url) {
        stage.classList.add('has-char-bg');
        stage.style.setProperty('--char-bg-image', 'url("' + String(url).replace(/"/g, '\\"') + '")');
      } else {
        stage.classList.remove('has-char-bg');
        stage.style.removeProperty('--char-bg-image');
      }
    } catch (_) {
      stage.classList.remove('has-char-bg');
      stage.style.removeProperty('--char-bg-image');
    }
  }

  // 暴露给 _tavern-session.js 的 stRenderSprite 尾部调用
  window.stRefreshImmerseBg = stRefreshImmerseBg;
  // 供抽屉打开钩子复用
  window.stImmerseBgUrl = stImmerseBgUrl;
})();
