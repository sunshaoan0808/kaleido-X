  async function stDeletePack(id) {
    if (!await showConfirm('删除 Pack？引用它的会话将变为只读。')) return;
    await stApi('/packs/' + encodeURIComponent(id), { method: 'DELETE' });
    await stLoadPacks();
    stStatus('Pack 已删除');
  }


  if ($('st-novel-file')) {
    $('st-novel-file').onchange = async (e) => {
      const file = e.target.files[0]; if (!file) return;
      const progress = $('st-import-progress');
      progress.classList.remove('hidden');
      try {
        const saved = await stImportNovel(file, file.name.replace(/\.[^.]+$/, ''));
        await stLoadPacks();
        stStatus('已导入 Pack：' + saved.title + '，共' + (saved.chapters || []).length + '章');
        e.target.value = '';
      } catch (err) {
        stStatus('导入失败：' + err.message);
      } finally {
        progress.classList.add('hidden');
      }
    };
  }
  if ($('st-pack-create')) $('st-pack-create').onclick = () => $('st-pack-editor').classList.remove('hidden');
  if ($('st-side-expand-all')) {
    $('st-side-expand-all').onclick = () => {
      const heads = document.querySelectorAll('#st-pack-list .st-pack-group-head, #st-session-list .st-session-group-head');
      const anyClosed = Array.from(heads).some(h => !h.classList.contains('open'));
      heads.forEach((h) => {
        const open = anyClosed; // expand all if any is closed, else collapse all
        h.classList.toggle('open', open);
        const g = h.parentElement;
        if (g) g.classList.toggle('open', open);
        h.setAttribute('aria-expanded', open ? 'true' : 'false');
      });
      const btn = $('st-side-expand-all');
      if (btn) btn.textContent = anyClosed ? '全部收起' : '全部展开';
    };
  }
  if ($('st-pack-cancel')) $('st-pack-cancel').onclick = () => $('st-pack-editor').classList.add('hidden');
  if ($('st-pack-save')) {
    $('st-pack-save').onclick = async () => {
      await stCreateEmptyPack();
      $('st-pack-editor').classList.add('hidden');
      $('st-pack-title').value = '';
      await stLoadPacks();
      stStatus('空 Pack 创建成功');
    };
  }
  if ($('st-chapter-edit')) {
    $('st-chapter-edit').onclick = () => {
      if (!tavernPack || !tavernPack.chapters || !tavernPack.chapters[0]) return;
      // toggle an inline editor
      const view = $('st-chapter-view');
      const pre = view.querySelector('pre');
      const editing = view.dataset.editing === '1';
      view.dataset.editing = editing ? '' : '1';
      if (editing) {
        // save
        const chId = view.dataset.chapterId;
        const content = (view.querySelector('textarea')?.value || '').trim();
        stApi('/packs/' + encodeURIComponent(tavernPack.id) + '/chapters/' + encodeURIComponent('chapters/' + chId + '.md'), { method: 'PUT', body: JSON.stringify({ content }) })
          .then(() => { stShowChapter(tavernPack.id, chId); stStatus('章节已保存'); })
          .catch(err => stStatus('保存失败：' + err.message));
      } else {
        // turn pre into textarea
        const txt = document.createElement('textarea');
        txt.rows = 10; txt.style.flex = '1';
        const oldText = pre.textContent; pre.innerHTML = ''; pre.appendChild(txt); txt.value = oldText;
      }
    };
  }


  function stShowChapter(packId, chId) {
    const p = tavernPacks.find(x => x.id === packId); if (!p) return;
    const ch = p.chapters.find(x => x.id === chId); if (!ch) return;
    tavernPack = p;
    stRenderLore();
    $('st-chapter-view').classList.remove('hidden');
    $('st-chapter-view').dataset.chapterId = chId;
    const pre = $('st-chapter-view').querySelector('pre');
    pre.textContent = '章节：' + ch.title + '\n节点：' + (ch.nodeIds || []).join('、') + '\n正文加载中…';
    stApi('/packs/' + encodeURIComponent(packId) + '/chapters/' + encodeURIComponent(ch.bodyPath))
      .then(body => {
        pre.textContent = (body.content || '').slice(0, 800);
      })
      .catch(e => { pre.textContent = '读取失败：' + e.message; });
  }

  // ----- Lore entries (ST-4b) -----
  let stLoreEditIdx = -1;
  function stEnsureLoreArray(pack) {
    if (!pack.loreEntries || !Array.isArray(pack.loreEntries)) pack.loreEntries = [];
    return pack.loreEntries;
  }
  function stRenderLore() {
    const panel = $('st-lore-panel');
    const list = $('st-lore-list');
    if (!panel || !list) return;
    if (!tavernPack) { panel.classList.add('hidden'); return; }
    panel.classList.remove('hidden');
    const entries = stEnsureLoreArray(tavernPack);
    list.innerHTML = '';
    if (!entries.length) {
      list.innerHTML = '<div class="muted sm">暂无 lore，可添加永久条或章范围条</div>';
      return;
    }
    entries.forEach((e, i) => {
      const el = document.createElement('div');
      el.className = 'item' + (stLoreEditIdx === i ? ' active' : '');
      const title = e.title || e.id || ('条目' + (i + 1));
      const meta = (e.permanent ? '永久' : (e.chapterRange || '无范围')) + (e.nodeIds && e.nodeIds.length ? ' · nodes ' + e.nodeIds.join(',') : '');
      el.innerHTML = '<span class="t"></span><small></small>';
      el.querySelector('.t').textContent = title;
      el.querySelector('small').textContent = meta;
      el.onclick = () => stOpenLoreEditor(i);
      list.appendChild(el);
    });
  }
  function stOpenLoreEditor(idx) {
    stLoreEditIdx = idx;
    $('st-lore-editor').classList.remove('hidden');
    const e = idx >= 0 ? stEnsureLoreArray(tavernPack)[idx] : { title: '', text: '', chapterRange: '', permanent: true, nodeIds: [] };
    $('st-lore-title').value = e.title || '';
    $('st-lore-text').value = e.text || e.content || '';
    $('st-lore-range').value = e.chapterRange || '';
    $('st-lore-perm').checked = !!e.permanent || !e.chapterRange;
    stRenderLore();
  }
  async function stSaveLore() {
    if (!tavernPack) return;
    const entries = stEnsureLoreArray(tavernPack);
    const entry = {
      id: (stLoreEditIdx >= 0 && entries[stLoreEditIdx].id) || ('lore-' + Date.now()),
      title: ($('st-lore-title').value || '').trim() || '未命名',
      text: ($('st-lore-text').value || '').trim(),
      chapterRange: ($('st-lore-range').value || '').trim(),
      permanent: !!$('st-lore-perm').checked,
      nodeIds: (stLoreEditIdx >= 0 && entries[stLoreEditIdx].nodeIds) || [],
    };
    if (stLoreEditIdx >= 0) entries[stLoreEditIdx] = entry; else entries.push(entry);
    tavernPack.loreEntries = entries;
    tavernPack.updatedAt = new Date().toISOString();
    // strip UI-only if any
    const payload = JSON.parse(JSON.stringify(tavernPack));
    delete payload.uploadChapters;
    await stApi('/packs', { method: 'POST', body: JSON.stringify(payload) });
    $('st-lore-editor').classList.add('hidden');
    stLoreEditIdx = -1;
    await stLoadPacks();
    const fresh = tavernPacks.find(p => p.id === payload.id);
    if (fresh) { tavernPack = fresh; }
    stRenderLore();
    stStatus('Lore 已保存 · ' + entry.title);
  }
