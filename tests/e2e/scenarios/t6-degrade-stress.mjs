// T6 FR6 复现·压力变体:按失败夜条件构造——
//  1) 会话代次churn:每 2 轮手机点 PC 卡重新拨号(不断开旧会话,等价真实场景连击);
//  2) 每 5 轮一次 90s WiFi 断网(等价"重探 10 次无回应"的网络事件,QUIC 60s idle 判死);
//  3) 断网恢复后按用户习惯由手机侧重新点连。
// 每轮 PC 推 96KB,断言弹窗+接收+done;失败即全链路取证退出。
//   node scenarios/t6-degrade-stress.mjs [--max 30] [--period-s 90]
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import createAdb from '../lib/adb.mjs';

const args = process.argv.slice(2);
const argOf = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? Number(args[i + 1]) : dflt;
};
const MAX_ROUNDS = argOf('--max', 30);
const PERIOD_S = argOf('--period-s', 90);

const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const runDir = join(e2eRoot(), 'reports', `t6-stress-${stamp}`);
mkdirSync(runDir, { recursive: true });
const log = (m) => console.log(`[${new Date().toISOString().slice(11, 19)}] ${m}`);
function assert(cond, msg) { if (!cond) throw new Error(msg); }
const sh = (cmd) => execFileSync('adb', ['-s', targets.huss_phone.adb.serial, 'shell', cmd], { timeout: 20000, encoding: 'utf8' });

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
  log(`!! 第 ${round} 轮失败: ${why} — 全链路取证`);
  try { await ch.screenshot(join(runDir, `v-fail-r${round}.png`)); } catch { }
  try { writeFileSync(join(runDir, `ui-fail-r${round}.json`), JSON.stringify(await ch.dump(), null, 1)); } catch { }
  try { writeFileSync(join(runDir, `phone-logcat-r${round}.log`), ch.logcat({ tagPrefix: ['LT'] })); } catch { }
  try { writeFileSync(join(runDir, `pc-logs-r${round}.log`), await pcLogsNonDiscovery()); } catch { }
  try { writeFileSync(join(runDir, `pc-state-r${round}.json`), JSON.stringify(await A.state(), null, 1)); } catch { }
}

/** 手机点 PC 卡(设备页),等会话重建(PC 状态 connected=true) */
async function phoneDialPc(fpPhone, fpPc) {
  await ch.tap({ testTag: `device-card-${fpPc}` });
  for (let i = 0; i < 12; i++) {
    await sleep(2000);
    try {
      const s = await A.state();
      if ((s.devices || []).some((d) => (d.id || d.fingerprint) === fpPhone && d.connected)) return true;
    } catch { }
  }
  return false;
}

async function onePush(round, fpPhone) {
  const t0 = Date.now();
  const name = `t6s-${stamp}-r${round}.bin`;
  const path = join(runDir, name);
  const b = Buffer.alloc(96 * 1024);
  b.write(`T6S ${stamp} r${round}`, 0, 'utf8'); b.fill(0x41 + (round % 26), 64);
  writeFileSync(path, b);
  const cardId = await A.invoke('push_files', { fingerprint: fpPhone, local_paths: [path.replace(/\//g, '\\')] });
  let accepted = false;
  for (let i = 0; i < 25 && !accepted; i++) {
    await sleep(1200);
    let els;
    try { els = await ch.dump(); } catch { continue; }
    const btn = els.find((e) => e.testTag === 'offer-accept-btn')
      || els.find((e) => /接收|接受|允许/.test((e.text || '') + (e.contentDesc || '')));
    if (btn) { await ch.tapXY(btn.center[0], btn.center[1]); accepted = true; }
  }
  if (!accepted) { await captureEvidence(round, 'OfferSheet 30s 未弹(入站断)'); return { ok: false, why: 'no-offer-sheet' }; }
  let pushed;
  try {
    pushed = await A.pollUntil((st) => {
      const t = (st.transfers || []).find((x) => x.id === cardId);
      return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
    }, { timeoutMs: 120_000, intervalMs: 1000, what: `r${round} 终态` });
  } catch (e) {
    await captureEvidence(round, '推送终态 120s 超时: ' + e.message);
    return { ok: false, why: 'transfer-timeout' };
  }
  try { rmSync(path, { force: true }); } catch { }
  if (pushed.value.state !== 'done') {
    await captureEvidence(round, `终态=${pushed.value.state}`);
    return { ok: false, why: 'state=' + pushed.value.state };
  }
  return { ok: true, secs: ((Date.now() - t0) / 1000).toFixed(1) };
}

try {
  log(`== T6 压力复现 runDir=${runDir} max=${MAX_ROUNDS} period=${PERIOD_S}s ==`);
  assert((await A.health()).bridgeReady, 'A bridgeReady');
  ch.wake(); ch.setStayOn(true); await sleep(1500);

  const s0 = await A.state();
  const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
  assert(phone, 'PC 应在线见到手机');
  const fpPhone = phone.id || phone.fingerprint;
  const fpPc = s0.selfDevice && s0.selfDevice.id ? s0.selfDevice.id : (s0.selfDevice && s0.selfDevice.fingerprint);
  log('手机指纹 ' + String(fpPhone).slice(0, 8) + ' PC指纹 ' + String(fpPc).slice(0, 8));

  // 手机停在设备页(弹窗全局覆盖;设备卡可点)
  try { await ch.tap({ testTag: 'nav-devices-link' }); } catch { }
  await sleep(1000);

  // 初始会话:手机点 PC 卡(失败夜同款方向)
  if (!await phoneDialPc(fpPhone, fpPc)) {
    // 回退:PC 侧 connect
    await A.invoke('connect', { fingerprint: fpPhone });
    await sleep(2000);
  }
  log('初始会话 ✓');

  const results = [];
  for (let round = 1; round <= MAX_ROUNDS; round++) {
    // 每 5 轮:90s 断网(QUIC 双端 idle 判死→全表清;恢复后手机侧重拨)
    if (round > 1 && (round - 1) % 5 === 0) {
      log(`r${round} 前置:WiFi 断网 90s(等价重探淘汰事件)`);
      sh('svc wifi disable');
      await sleep(90_000);
      sh('svc wifi enable');
      // 等发现恢复(PC 见手机 online)
      let back = false;
      for (let i = 0; i < 20 && !back; i++) {
        await sleep(3000);
        try {
          const s = await A.state();
          back = (s.devices || []).some((d) => (d.id || d.fingerprint) === fpPhone && d.online);
        } catch { }
      }
      log(`r${round} WiFi 恢复(发现${back ? '✓' : '✗ 20轮未见'}) → 手机重拨`);
      await sleep(5000);
      const ok = await phoneDialPc(fpPhone, fpPc);
      log(`r${round} 重拨 ${ok ? '✓' : '✗(仍推,可能暴露陈旧会话)'}`);
      await sleep(3000);
    } else if (round > 1 && round % 2 === 0) {
      // 每 2 轮:PC 侧 connect 重拨(不断旧会话,代次churn——connect 命令
      // 不查表直接新建连接,与失败夜"多次重连"等价的会话覆盖压力)
      log(`r${round} 前置:PC connect 重拨(churn)`);
      try { await A.invoke('connect', { fingerprint: fpPhone }); } catch (e) { log('connect: ' + e.message); }
      await sleep(1500);
    }

    const r = await onePush(round, fpPhone);
    results.push({ round, ...r });
    if (r.ok) log(`r${round} PASS ${r.secs}s (累计 ${results.length} 轮, PASS ${results.filter(x => x.ok).length})`);
    else break;
    await sleep(Math.max(0, PERIOD_S * 1000 - 15000));
  }

  const pass = results.filter((r) => r.ok).length;
  log(`== 结束: ${pass}/${results.length} 轮 PASS ==`);
  writeFileSync(join(runDir, 'summary.json'), JSON.stringify({ stamp, results }, null, 1));
  if (pass < results.length) process.exitCode = 1;
} catch (e) {
  console.error('FAIL:', e.message);
  process.exitCode = 1;
}
