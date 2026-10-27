let sessionId = null;
let seeking = false;
document.getElementById('seek').addEventListener('input', () => seeking = true);
document.getElementById('seek').addEventListener('change', async () => {
  if (!sessionId) return;
  const v = Number(document.getElementById('seek').value) / 1000;
  const dur = Number(document.getElementById('dur').dataset.sec || 0);
  try { await rpc('seek', { session_id: sessionId, position_sec: v * dur }); }
  catch (e) { msg(e.message); }
  seeking = false;
});
document.getElementById('vol').addEventListener('change', async () => {
  if (!sessionId) return;
  const v = Number(document.getElementById('vol').value) / 100;
  try { await rpc('set_volume', { session_id: sessionId, volume: v }); }
  catch (e) { msg(e.message); }
});

function setPhase(p) {
  document.getElementById('phase').textContent = p;
  const playing = p === 'playing' || p === 'paused';
  document.getElementById('btnPause').disabled = p !== 'playing';
  document.getElementById('btnResume').disabled = p !== 'paused';
  document.getElementById('btnStop').disabled = !playing;
  document.getElementById('seek').disabled = !playing;
  document.getElementById('vol').disabled = !playing;
  if (!playing) document.getElementById('sub').disabled = true;
}
async function subSelectChanged() {
  if (!sessionId) return;
  const v = Number(document.getElementById('sub').value);
  msg('');
  try { await rpc('set_subtitle', { session_id: sessionId, track: v }); }
  catch (e) { msg(e.message); }
}
function fillSubs(st) {
  const sel = document.getElementById('sub');
  const tracks = st.subtitle_tracks || [];
  const cur = String(st.subtitle_track !== undefined ? st.subtitle_track : -1);
  const sig = JSON.stringify(tracks);
  if (sel.dataset.sig !== sig + '|' + cur) {
    sel.dataset.sig = sig + '|' + cur;
    sel.innerHTML = '';
    const off = document.createElement('option');
    off.value = '-1'; off.textContent = '关闭';
    if (cur === '-1') off.selected = true;
    sel.appendChild(off);
    for (const t of tracks) {
      const o = document.createElement('option');
      o.value = String(t.id);
      o.textContent = '#' + t.id + ' ' + (t.lang || '') + (t.title ? ' · ' + t.title : '');
      if (cur === String(t.id)) o.selected = true;
      sel.appendChild(o);
    }
  }
  sel.disabled = !(st.playing || st.phase === 'paused') || st.kind === 'viewer';
}
async function play() {
  const t = document.getElementById('target').value.trim();
  if (!t) { msg('先填 placement_id 或 content_id'); return; }
  msg('');
  try {
    const args = /^\d+$/.test(t) ? { placement_id: Number(t) } : { content_id: t };
    const r = await rpc('play', args);
    sessionId = r.session_id;
    setPhase('playing');
    if (r.content_len) document.getElementById('dur').textContent = fmt(r.content_len > 1e6 ? r.content_len : 0);
  } catch (e) { msg(e.message); }
}
async function control(method) {
  if (!sessionId) return;
  msg('');
  try {
    await rpc(method, { session_id: sessionId });
    if (method === 'stop') setPhase('stopped');
    else if (method === 'pause') setPhase('paused');
    else if (method === 'resume') setPhase('playing');
  } catch (e) { msg(e.message); }
}
async function loadOutputs() {
  try {
    const out = await rpc('outputs.list', {});
    const box = document.getElementById('outputs');
    box.innerHTML = '';
    for (const r of out.rows) {
      const b = document.createElement('button');
      if (r.active) b.className = 'active';
      b.textContent = (r.kind === 'display' ? '显示 · ' : '音频 · ') + r.name;
      b.onclick = async () => {
        try {
          await rpc('outputs.set', { output_id: r.id });
          await loadOutputs();
        } catch (e) { msg(e.message); }
      };
      box.appendChild(b);
    }
    if (!out.rows.length) box.innerHTML = '<span class="hint">未探测到输出设备</span>';
  } catch (e) { msg(e.message); }
}
async function loadReceivers() {
  try {
    const out = await rpc('receiver.list', {});
    const box = document.getElementById('receivers');
    box.innerHTML = '';
    for (const r of out.rows) {
      const b = document.createElement('button');
      if (r.state === 'playing' || r.state === 'paused') b.className = 'active';
      b.textContent = r.name + ' · ' + r.addr + ':' + r.port + ' [' + r.state + '] → 投屏';
      b.onclick = async () => {
        if (!sessionId) { msg('先打开一个视频会话再投屏'); return; }
        msg('');
        try {
          const mode = document.getElementById('castMode').value;
          const args = { session_id: sessionId, target_device: r.id };
          if (mode) args.mode = mode; // 空 = 自动路由（P4 §5.3.2）
          const res = await rpc('cast', args);
          document.getElementById('btnCastStop').disabled = false;
          msg('已投屏 ' + res.receiver_id + '（模式 ' + res.mode + '）');
        } catch (e) { msg(e.message); }
      };
      box.appendChild(b);
    }
    if (!out.rows.length) box.innerHTML = '<span class="hint">未发现接收器（player-lite 未运行或不在同一局域网）</span>';
  } catch (e) { msg(e.message); }
}
async function stopCast() {
  if (!sessionId) { msg('无会话'); return; }
  msg('');
  try {
    const r = await rpc('cast.stop', { session_id: sessionId });
    if (r.stopped) document.getElementById('btnCastStop').disabled = true;
    msg(r.stopped ? '已停止投屏' : '该会话没有进行中的投屏');
  } catch (e) { msg(e.message); }
}
async function poll() {
  if (sessionId) {
    try {
      const out = await rpc('status', { session_id: sessionId });
      const st = out.sessions && out.sessions[0];
      if (st) {
        setPhase(st.phase);
        fillSubs(st);
        if (st.kind === 'viewer') {
          document.getElementById('status').textContent = '图片/PDF 会话 → 查看器页';
          document.getElementById('sub').disabled = true;
        } else {
          document.getElementById('status').textContent = st.asset_type || '';
        }
        if (!seeking) {
          document.getElementById('pos').textContent = fmt(st.position_sec);
          const dur = document.getElementById('dur');
          dur.dataset.sec = st.duration_sec;
          dur.textContent = fmt(st.duration_sec);
          if (st.duration_sec > 0) {
            document.getElementById('seek').value = Math.round(st.position_sec / st.duration_sec * 1000);
          }
        }
      }
    } catch (e) { /* 会话可能已被移除 */ }
  }
  setTimeout(poll, 1000);
}
document.getElementById('btnPlay').onclick = play;
document.getElementById('btnPause').onclick = () => control('pause');
document.getElementById('btnResume').onclick = () => control('resume');
document.getElementById('btnStop').onclick = () => control('stop');
document.getElementById('btnCastStop').onclick = stopCast;
document.getElementById('sub').onchange = subSelectChanged;
loadOutputs();
loadReceivers();
setInterval(loadReceivers, 3000);
poll();
