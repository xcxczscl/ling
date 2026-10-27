/* player 控制面共享脚本（rpc 网关 / 消息 / 格式化；control/viewer 同源） */
const SHELL = 'p2';

async function rpc(method, params) {
  const res = await fetch('/rpc', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ plugin: 'player', method: 'player.' + method, params })
  }).then(r => r.json());
  if (!res.ok) { const e = res.error || {}; throw new Error((e.code||'E') + ' ' + (e.message||'')); }
  return res.result;
}
function msg(t) { const el = document.getElementById('msg'); if (el) el.textContent = t || ''; }
function fmt(s) { return (Math.round(s * 10) / 10).toFixed(1) + 's'; }
