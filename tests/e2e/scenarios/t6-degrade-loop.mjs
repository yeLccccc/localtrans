// T6 FR6 复现循环:PC 每轮推一个小文件给手机,断言 OfferSheet 弹出+接收+done。
// 循环直到失败(带全链路 T6P 探针取证)或跑满 --max 轮。用法:
//   node scenarios/t6-degrade-loop.mjs [--max 40] [--period-s 90]
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import createAdb from '../lib/adb.mjs';

const args = process.argv.slice(2);
const argOf = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? Number(args[i + 1]) : dflt;
};
const MAX_ROUNDS = argOf('--max', 40);
const PERIOD_S = argOf('--period-s', 90);

const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const runDir = join(e2eRoot(), 'reports', `t6-loop-${stamp}`);
mkdirSync(runDir, { recursive: true });
const log = (m) => console.log(`[${new Date().toISOString().slice(11, 19)}] ${m}`);

function assert(cond, msg) { if (!cond) throw new Error(msg); }

async function pcLogsNonDiscovery() {
  try {
    const all = await A.logs({ afterSeq: 0 });
    const es = (all && all.entries) || [];
    return es.filter((e) => !/discovery/.test(e.target || ''))
      .map((e) => `${e.seq} ${e.level} ${e.target} | ${e.message}`)
      .join('\n');
  } catch (e) { return `pcLogs 失败: ${e.message}`; }
}

async function captureEvidence(round, why) {
  log(`!! 第 ${round} 轮失败: ${why} — 取证`);
  try {
    await ch.screenshot(join(runDir, `v-fail-r${round}.png`));
  } catch { }
  try {
    const els = await ch.dump();
    writeFileSync(join(runDir, `ui-fail-r${round}.json`), JSON.stringify(els, null, 1));
  } catch { }
  try {
    writeFileSync(join(runDir, `phone-logcat-r${round}.log`), ch.logcat({ tagPrefix: ['LT'] }));
  } catch { }
  try {
    writeFileSync(join(runDir, `pc-logs-r${round}.log`), await pcLogsNonDiscovery());
  } catch { }
  try {
    const st = await A.state();
    writeFileSync(join(runDir, `pc-state-r${round}.json`), JSON.stringify(st, null, 1));
  } catch { }
}

try {
  log(`== T6 退化复现循环 runDir=${runDir} max=${MAX_ROUNDS} period=${PERIOD_S}s ==`);
  assert((await A.health()).bridgeReady, 'A bridgeReady');
  ch.wake(); ch.setStayOn(true); await sleep(1500);

  // 手机指纹(PC 侧设备表)
  const s0 = await A.state();
  const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
  assert(phone, 'PC 应在线见到手机: ' + JSON.stringify((s0.devices || []).map((d) => d.name)));
  const fpPhone = phone.id || phone.fingerprint;
  log('手机指纹 ' + String(fpPhone).slice(0, 8) + ' @ ' + phone.addr);

  // 建立会话(等价场景"手机点 PC 卡"后的受信直连;这里 PC 侧主动 connect)
  let connected = false;
  for (let i = 0; i < 3 && !connected; i++) {
    try { await A.invoke('connect', { fingerprint: fpPhone }); } catch (e) { log('connect 尝试失败: ' + e.message); }
    await sleep(2000);
    const s = await A.state();
    connected = (s.devices || []).some((d) => (d.id || d.fingerprint) === fpPhone && d.connected);
  }
  log('会话建立 ' + (connected ? '✓' : '✗(仍继续,首轮会暴露)'));

  const results = [];
  for (let round = 1; round <= MAX_ROUNDS; round++) {
    const t0 = Date.now();
    const name = `t6p-${stamp}-r${round}.bin`;
    const path = join(runDir, name);
    const b = Buffer.alloc(96 * 1024);
    b.write(`T6 ${stamp} r${round}`, 0, 'utf8'); b.fill(0x50 + (round % 26), 64);
    writeFileSync(path, b);

    const cardId = await A.invoke('push_files', { fingerprint: fpPhone, local_paths: [path.replace(/\//g, '\\')] });
    const stNow = await A.state();
    const dev = (stNow.devices || []).find((d) => (d.id || d.fingerprint) === fpPhone);
    log(`r${round} push → card=${cardId} pcConnected=${dev ? dev.connected : '?'}`);

    // 等弹窗(30s)
    let accepted = false;
    for (let i = 0; i < 25 && !accepted; i++) {
      await sleep(1200);
      let els;
      try { els = await ch.dump(); } catch { continue; }
      const btn = els.find((e) => e.testTag === 'offer-accept-btn')
        || els.find((e) => /接收|接受|允许/.test((e.text || '') + (e.contentDesc || '')));
      if (btn) { await ch.tapXY(btn.center[0], btn.center[1]); accepted = true; }
    }
    if (!accepted) {
      await captureEvidence(round, 'OfferSheet 30s 未弹(入站断)');
      results.push({ round, ok: false, why: 'no-offer-sheet' });
      break;
    }

    // 等终态(120s)
    let pushed;
    try {
      pushed = await A.pollUntil((st) => {
        const t = (st.transfers || []).find((x) => x.id === cardId);
        return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
      }, { timeoutMs: 120_000, intervalMs: 1000, what: `r${round} 终态` });
    } catch (e) {
      await captureEvidence(round, '推送终态 120s 超时: ' + e.message);
      results.push({ round, ok: false, why: 'transfer-timeout' });
      break;
    }
    const secs = ((Date.now() - t0) / 1000).toFixed(1);
    if (pushed.value.state !== 'done') {
      await captureEvidence(round, `终态=${pushed.value.state}`);
      results.push({ round, ok: false, why: 'state=' + pushed.value.state });
      break;
    }
    results.push({ round, ok: true, secs });
    log(`r${round} PASS done ${secs}s (累计 ${results.length} 轮)`);
    try { rmSync(path, { force: true }); } catch { }

    if (round < MAX_ROUNDS) await sleep(Math.max(0, PERIOD_S * 1000 - (Date.now() - t0)));
  }

  const pass = results.filter((r) => r.ok).length;
  log(`== 结束: ${pass}/${results.length} 轮 PASS ==`);
  writeFileSync(join(runDir, 'summary.json'), JSON.stringify({ stamp, results }, null, 1));
  if (pass < results.length) process.exitCode = 1;
} catch (e) {
  console.error('FAIL:', e.message);
  try { writeFileSync(join(runDir, 'summary.json'), JSON.stringify({ stamp, error: e.message }, null, 1)); } catch { }
  process.exitCode = 1;
}
