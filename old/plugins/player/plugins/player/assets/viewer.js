let sessionId = null;
let assetType = null;

async function openContent() {
  const t = document.getElementById('target').value.trim();
  if (!t) { msg('先填 placement_id 或 content_id'); return; }
  msg('');
  try {
    if (sessionId) { try { await rpc('stop', { session_id: sessionId }); } catch (e) {} sessionId = null; }
    const args = /^\d+$/.test(t) ? { placement_id: Number(t) } : { content_id: t };
    const r = await rpc('play', args);
    assetType = r.asset_type;
    if (assetType !== 'image' && assetType !== 'doc') {
      sessionId = r.session_id;
      await rpc('stop', { session_id: sessionId }).catch(() => {});
      sessionId = null;
      msg('该内容是 ' + assetType + '，请用「播放器」控制页');
      document.getElementById('frameBox').innerHTML =
        '<span class="hint">视频/音频在播放器控制页打开。</span>';
      return;
    }
    sessionId = r.session_id;
    document.getElementById('btnStop').disabled = false;
    await refreshFrame();
    await refreshMeta();
  } catch (e) { msg(e.message); }
}

async function refreshFrame() {
  if (!sessionId) return;
  try {
    const f = await rpc('frame', { session_id: sessionId });
    document.getElementById('frameBox').innerHTML =
      '<img alt="frame" src="data:' + f.mime + ';base64,' + f.data + '">';
  } catch (e) { msg(e.message); }
}

async function refreshMeta() {
  if (!sessionId) return;
  try {
    const out = await rpc('status', { session_id: sessionId });
    const st = out.sessions && out.sessions[0];
    if (!st) return;
    const isPdf = st.asset_type === 'doc';
    document.getElementById('meta').textContent = isPdf
      ? ('第 ' + st.page + ' / ' + st.pages + ' 页')
      : (st.width + '×' + st.height + ' px');
    document.getElementById('btnPrev').disabled = !isPdf || st.page <= 1;
    document.getElementById('btnNext').disabled = !isPdf || st.page >= st.pages;
  } catch (e) { /* 会话可能已移除 */ }
}

async function nav(delta) {
  if (!sessionId) return;
  msg('');
  try {
    const out = await rpc('status', { session_id: sessionId });
    const st = out.sessions[0];
    const next = st.page + delta;
    await rpc('goto_page', { session_id: sessionId, page: next });
    await refreshFrame();
    await refreshMeta();
  } catch (e) { msg(e.message); }
}

async function stopContent() {
  if (!sessionId) return;
  try { await rpc('stop', { session_id: sessionId }); } catch (e) {}
  sessionId = null; assetType = null;
  document.getElementById('btnStop').disabled = true;
  document.getElementById('btnPrev').disabled = true;
  document.getElementById('btnNext').disabled = true;
  document.getElementById('meta').textContent = '–';
  document.getElementById('frameBox').innerHTML =
    '<span class="hint">已停止。</span>';
}

document.getElementById('btnOpen').onclick = openContent;
document.getElementById('btnPrev').onclick = () => nav(-1);
document.getElementById('btnNext').onclick = () => nav(1);
document.getElementById('btnStop').onclick = stopContent;
