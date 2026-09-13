// smoke-m5: 双机部署链路 + 互发现冒烟（M5 验收）
// 前置：huss_pc(开发机,127.0.0.1:39872) 与 huss_laptop(测试机,:39871) 均已按部署流程启动
// 用法：node smoke-m5.mjs
import { mkdirSync, writeFileSync } from 'node:fs';
import { loadTargets } from './lib/config.mjs';

const targets = loadTargets();
const huss_pc = { ...targets.huss_pc, expectPeer: 'huss_laptop' };
const huss_laptop = { ...targets.huss_laptop, expectPeer: 'huss_pc' };

const api = async (t, path, opts = {}) => {
  const r = await fetch(`http://${t.host}:${t.port}${path}`, {
    ...opts,
    headers: { Authorization: `Bearer ${t.token}`, ...(opts.headers || {}) },
  });
  const ct = r.headers.get('content-type') || '';
  return { status: r.status, json: ct.includes('json') ? await r.json() : null,
           bin: ct.includes('png') ? Buffer.from(await r.arrayBuffer()) : null };
};
const wait = (ms) => new Promise(r => setTimeout(r, ms));
const results = [];
const step = async (name, fn) => {
  try { const detail = await fn(); results.push(['PASS', name, detail]); console.log(`  PASS  ${name}${detail ? '  ' + detail : ''}`); }
  catch (e) { results.push(['FAIL', name, e.message]); console.log(`  FAIL  ${name}  ${e.message}`); }
};
const assert = (c, msg) => { if (!c) throw new Error(msg); };

const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 15);
const outDir = new URL(`./reports/m5-${ts}/`, import.meta.url).href.replace('file:///', '');
mkdirSync(outDir, { recursive: true });

console.log('[smoke-m5] 双机互发现冒烟 开始');

await step('双端 health + version', async () => {
  for (const [n, t] of [['huss_pc', huss_pc], ['huss_laptop', huss_laptop]]) {
    const h = await api(t, '/api/health');
    assert(h.status === 200 && h.json?.data?.status === 'ok', `${n} health 异常`);
    const v = await api(t, '/api/version');
    assert(v.json?.data?.bridgeReady === true, `${n} bridge 未就绪`);
    assert(v.json?.data?.appVersion === '0.12.0', `${n} 版本 ${v.json?.data?.appVersion}`);
  }
  return '双端 bridgeReady=true, v0.12.0';
});

let runA, runB;
await step('双端 test/begin', async () => {
  runA = (await api(huss_pc, '/api/test/begin', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ scenario: 'smoke-m5-discovery' }) })).json?.data?.runId;
  runB = (await api(huss_laptop, '/api/test/begin', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ scenario: 'smoke-m5-discovery' }) })).json?.data?.runId;
  assert(runA && runB, 'runId 缺失');
  return `runA=${runA.slice(0, 8)}… runB=${runB.slice(0, 8)}…`;
});

await step('state/wait 双向发现断言', async () => {
  const q = { method: 'POST', headers: { 'content-type': 'application/json' }, body: '' };
  const wA = await api(huss_pc, `/api/state/wait`, { ...q, body: JSON.stringify({ source: 'transfers', path: 'devices.length', op: 'gte', value: 1, timeoutMs: 15000 }) });
  assert(wA.status === 200, `huss_pc 等待超时/失败: ${JSON.stringify(wA.json)}`);
  const wB = await api(huss_laptop, `/api/state/wait`, { ...q, body: JSON.stringify({ source: 'transfers', path: 'devices.length', op: 'gte', value: 1, timeoutMs: 15000 }) });
  assert(wB.status === 200, `huss_laptop 等待超时/失败: ${JSON.stringify(wB.json)}`);
  const namesA = wA.json.data.observed?.map ? '' : '';
  const sA = (await api(huss_pc, '/api/state/transfers')).json.data;
  const sB = (await api(huss_laptop, '/api/state/transfers')).json.data;
  const aSee = sA.devices.map(d => d.name).join(',');
  const bSee = sB.devices.map(d => d.name).join(',');
  assert(sA.devices.some(d => d.name === huss_pc.expectPeer), `huss_pc 未发现 ${huss_pc.expectPeer}，实际: ${aSee}`);
  assert(sB.devices.some(d => d.name === huss_laptop.expectPeer), `huss_laptop 未发现 ${huss_laptop.expectPeer}，实际: ${bSee}`);
  return `huss_pc 看到[${aSee}] huss_laptop 看到[${bSee}]`;
});

await step('双端截图留证', async () => {
  for (const [n, t] of [['huss_pc', huss_pc], ['huss_laptop', huss_laptop]]) {
    const s = await api(t, '/api/screenshot');
    assert(s.bin && s.bin.slice(1, 4).toString() === 'PNG' && s.bin.length > 10000, `${n} 截图异常`);
    writeFileSync(outDir + `${n}.png`, s.bin);
  }
  return `已存 ${outDir}{huss_pc,huss_laptop}.png`;
});

await step('双端日志按 runId 提取', async () => {
  for (const [n, t, rid] of [['huss_pc', huss_pc, runA], ['huss_laptop', huss_laptop, runB]]) {
    const l = await api(t, `/api/logs/tail?runId=${rid}`);
    assert(l.json?.data?.entries?.length >= 1, `${n} runId 日志为空`);
  }
  return '双端 runId 过滤有效';
});

await step('双端 test/end', async () => {
  for (const [n, t, rid] of [['huss_pc', huss_pc, runA], ['huss_laptop', huss_laptop, runB]]) {
    const e = await api(t, '/api/test/end', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ runId: rid, outcome: 'pass' }) });
    assert(e.json?.ok, `${n} end 失败`);
  }
  return 'ok';
});

const pass = results.filter(r => r[0] === 'PASS').length;
console.log(`\n[smoke-m5] ${pass}/${results.length} 步骤通过`);
writeFileSync(outDir + 'summary.json', JSON.stringify({ ts, results }, null, 2));
process.exit(pass === results.length ? 0 : 1);
