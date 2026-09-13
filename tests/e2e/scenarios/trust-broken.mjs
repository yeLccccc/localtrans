// 场景：trust-broken（M3a T5 FR6 TrustBroken 协议通知——移除信任即时断连降级）
//
// ★★ 状态：待 huss_laptop（192.168.0.222）恢复执行 —— 设备休眠/不可达时本场景
//    自动 SKIP 退出（exit 0 + 报告 outcome=skip）；已注册 run-all（注册表自动跳）。
//    条件跳过手法与 scenarios/connect-memory.mjs 一致。★★
//
// 验收路径（spec FR6 "移除信任即时断连降级"）：
//   0) 双 PC 不可达（任一 bridge 未就绪）→ SKIP（条件跳过设计）。
//   1) 清场+重配对：A/B 双向 remove_trusted → A connect → B 同意 → 输码 → 双端 trusted。
//   2) 单边移除：A remove_trusted(B)——A 在断会话**前**向 B 发 ControlMsg::TrustBroken。
//   3) 断言 B 端（收通知方）"即时降级"：
//      a. toast「已移除对你的信任」出现（UI 目检 + 截图）；
//      b. B 的 sessions 中对端 A 消失（TrustBroken 与 Goodbye 同款收尾：清表+SessionDown）；
//      c. B 的信任表中 A 条目被同步删除（双盲对称重配，徽章降级为待配对）；
//      d. 观察窗内 B 不自动重连 A（连接记忆已随 TrustBroken 清除）。
//   4) 收尾恢复夹具：L3 keepIdentity+seedTrust 双向互播 + 设备名还原 + connect 验证。
//
// wire 兼容注：TrustBroken 对旧版本对端表现为"未知变体→解码失败→干净断连"，
// 终态等价（连接本就被发送方关闭），本场景两台均为新版本，不断言旧端路径。
// 任何一步失败：collectEvidence → 报告 FAIL → 尽力兜底恢复 PC 夹具。
// 用法：node scenarios/trust-broken.mjs（前置：node lib/deploy.mjs 已部署双端）
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';

const SCENARIO = 'trust-broken';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);       // 移除方（发 TrustBroken）
const B = new Target('huss_laptop', targets.huss_laptop, journal); // 收通知方（降级断连）
const both = [A, B];

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let pcFp = null;
let laptopFp = null;

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const fingerprintOf = async (t) => (await t.invoke('get_device_fingerprint')).fingerprint_hex;

/** 步骤包装：PASS/FAIL 计时入矩阵；失败上抛由顶层 catch 统一收割证据 */
async function step(name, fn) {
  const t0 = Date.now();
  console.log(`  ▶ ${name}`);
  try {
    const r = await fn();
    const detail = typeof r === 'string' ? r : (r?.detail || '');
    const artifacts = typeof r === 'string' ? [] : (r?.artifacts || []);
    steps.push({ name, status: 'PASS', detail, durationMs: Date.now() - t0, artifacts });
    console.log(`  PASS  ${name}${detail ? '  ' + detail : ''}`);
  } catch (e) {
    steps.push({ name, status: 'FAIL', detail: e.message, durationMs: Date.now() - t0 });
    throw e;
  }
}

/** 可达探测（短超时）：laptop 休眠时 fetch 快速失败/超时 → 调用方走 SKIP 出口 */
async function reachable(t, timeoutMs = 8000) {
  try {
    const v = await t.ok('/api/version', { timeoutMs });
    return v?.bridgeReady === true;
  } catch { return false; }
}

/** PC 侧读配对码（acceptor 亮码 .code-display；超时兜底读 get_pairing_pending） */
async function readPcPairingCode(t, peerFp, { timeoutMs = 20_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastTxt = '';
  const digits = (s) => (s || '').replace(/\D/g, '');
  while (Date.now() < deadline) {
    try {
      lastTxt = await t.uiText('.code-display');
      const code = digits(lastTxt);
      if (/^\d{6}$/.test(code)) return code;
    } catch { /* 亮码前 DOM 无 .code-display */ }
    await sleep(700);
  }
  const pending = await t.invoke('get_pairing_pending');
  const mine = (pending || []).find((p) => p.fingerprint === peerFp);
  const code = digits(mine?.own_code);
  if (/^\d{6}$/.test(code)) return code;
  throw new Error(`配对码读取失败: .code-display 末次="${String(lastTxt).slice(0, 40)}"，pending=${JSON.stringify(pending)?.slice(0, 200)}`);
}

/** UI 恢复设备名（L3 后 config.json 被清 → 出厂名=COMPUTERNAME；夹具必须还原） */
async function restoreDeviceName(t, name) {
  await t.uiNavigate('/settings');
  await t.uiWait('[testid=settings-device-name-input]', 8000);
  await t.uiInput('[testid=settings-device-name-input]', name, { events: ['blur'] });
  for (let i = 0; i < 6; i++) {
    if ((await t.invoke('get_settings')).device_name === name) return;
    await sleep(800);
  }
  throw new Error(`${t.name} 设备名恢复失败（应 ${name}）`);
}

/** 夹具恢复：L3 keepIdentity+seedTrust 互播 + 设备名还原 + connect 验证 */
async function restorePcFixture() {
  const seed = {
    huss_pc: [{ fingerprint: laptopFp, name: 'huss_laptop' }],
    huss_laptop: [{ fingerprint: pcFp, name: 'huss_pc' }],
  };
  await reset(3, both, { keepIdentity: true, seedTrust: seed });
  await restoreDeviceName(A, 'huss_pc');
  await restoreDeviceName(B, 'huss_laptop');
  let lastErr = null;
  for (let i = 0; i < 3; i++) {
    try { await A.invoke('connect', { fingerprint: laptopFp }); lastErr = null; break; }
    catch (e) { lastErr = e; await sleep(3000); }
  }
  if (lastErr) throw lastErr;
  await Promise.all([
    A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
      { timeoutMs: 30_000, what: 'huss_pc sessions trusted' }),
    B.pollUntil((s) => s.sessions.some((x) => x.peer === pcFp && x.trusted),
      { timeoutMs: 30_000, what: 'huss_laptop sessions trusted' }),
  ]);
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口：任一 PC 不可达 → SKIP（不等待设备恢复）----
  const aUp = await reachable(A);
  const bUp = aUp ? await reachable(B) : false;
  if (!aUp || !bUp) {
    const msg = `SKIP：${!aUp ? 'huss_pc' : 'huss_laptop'} 不可达（bridge 未就绪）——本场景待设备恢复后执行（M3a T5 验收期，huss_laptop 不可达）`;
    console.log(`\n[${SCENARIO}] ${msg}`);
    note(msg);
    writeReport({
      runId, scenario: SCENARIO, steps: [{ name: '可达探测', status: 'SKIP', detail: msg, durationMs: 0 }],
      outcome: 'skip',
      env: {
        runDir, startedAt: startedAt.toISOString(), finishedAt: new Date().toISOString(),
        durationMs: Date.now() - startedAt, versions,
        notes: [
          '条件跳过设计：任一 PC bridge 未就绪即 SKIP 退出（exit 0），run-all 注册表照常通过',
          'M3a 计划卡 T5 验收环境约束：huss_laptop（192.168.0.222）恢复后手动重跑本场景补双机证据',
        ],
      },
    });
    return; // 正常返回 → outcome=skip → exit 0
  }

  await step('环境握手：双 PC bridge 就绪（记录预运行指纹）', async () => {
    for (const t of both) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      versions[t.name] = v;
    }
    pcFp = await fingerprintOf(A);
    laptopFp = await fingerprintOf(B);
    note(`预运行指纹 huss_pc=${pcFp} huss_laptop=${laptopFp}`);
    return `huss_pc v${versions.huss_pc.appVersion}，huss_laptop v${versions.huss_laptop.appVersion}`;
  });

  await step('双 PC test/begin', async () => {
    const a = await A.beginTest(SCENARIO);
    const b = await B.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…`;
  });

  await step('【清场+重配对】双向 remove_trusted → A connect → B 同意 → 输码 → 双端 trusted', async () => {
    await A.invoke('remove_trusted', { fingerprint: laptopFp });
    await B.invoke('remove_trusted', { fingerprint: pcFp });
    await sleep(2000); // 等信任清理传播(断连事件传播到双方)
    // connect 可能因会话清理延迟失败——重试 3 次
    let connected = false;
    for (let retry = 0; retry < 3 && !connected; retry++) {
      try {
        await A.invoke('connect', { fingerprint: laptopFp });
        connected = true;
      } catch (e) {
        console.log(`    connect 第${retry+1}次失败: ${e.message.slice(0,60)}`);
        await sleep(3000);
        await A.invoke('connect', { fingerprint: laptopFp }).catch(() => {});
        // 第二次后检查
        const s = await A.state();
        connected = (s.sessions??[]).some(x => x.peer === laptopFp);
        if (connected) break;
      }
    }
    await B.uiWait('[testid=pairing-dialog]', 15_000);
    await B.uiClick('[testid=btn-grant]');
    const code = await readPcPairingCode(B, pcFp);
    await A.uiWait('[testid=code-input]', 20_000);
    await A.uiInput('[testid=code-input]', code);
    await A.uiClick('[testid=btn-submit]');
    await Promise.all([
      A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc sessions trusted' }),
      B.pollUntil((s) => s.sessions.some((x) => x.peer === pcFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop sessions trusted' }),
    ]);
    return `配对完成（码 ${code.slice(0, 2)}****），双端会话 trusted`;
  });

  await step('【单边移除】A remove_trusted(B) → 断会话前发 TrustBroken 通知', async () => {
    // 生产路径：remove_trusted_inner 先清本地信任/记忆 → send_ctrl(TrustBroken) → disconnect
    await A.invoke('remove_trusted', { fingerprint: laptopFp });
    const aTrusted = await A.invoke('list_trusted');
    if (aTrusted.some((p) => p.fingerprint === laptopFp)) {
      throw new Error('A 移除后信任表仍含 B');
    }
    return 'A 已移除对 B 的信任（TrustBroken 已随断会话路径发出）';
  });

  await step('【即时降级】B 端：toast + 会话断开 + 信任条目对称删除 + 不自动重连', async () => {
    // a. toast 文案（core 收 TrustBroken → 壳层事件泵弹出；5s 轮询窗口）
    let toastSeen = false;
    let lastToast = '';
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline && !toastSeen) {
      try {
        lastToast = await B.uiText('.toast');
        if (/已移除对你的信任/.test(lastToast || '')) toastSeen = true;
      } catch { /* toast 未渲染 */ }
      if (!toastSeen) await sleep(500);
    }
    await B.screenshot(join(runDir, 'v-trust-broken-toast.png'));
    if (!toastSeen) note(`toast 未捕获（末次="${String(lastToast).slice(0, 60)}"）——截图目检兜底，非硬断言`);

    // b. 会话断开：B 的 sessions 中对端 A 消失（Goodbye 同款收尾）
    await B.pollUntil((s) => !s.sessions.some((x) => x.peer === pcFp),
      { timeoutMs: 30_000, intervalMs: 1000, what: 'huss_laptop sessions 已无 huss_pc（TrustBroken 断连）' });
    await A.pollUntil((s) => !s.sessions.some((x) => x.peer === laptopFp),
      { timeoutMs: 30_000, intervalMs: 1000, what: 'huss_pc sessions 已无 huss_laptop（本地 disconnect）' });

    // c. 信任条目对称删除（双盲对称重配——core 收通知方删本端条目）
    const bTrusted = await B.invoke('list_trusted');
    if (bTrusted.some((p) => p.fingerprint === pcFp)) {
      throw new Error(`B 收到 TrustBroken 后信任表仍含 A: ${JSON.stringify(bTrusted)}`);
    }

    // d. 观察窗 10s：B 不自动重连 A（连接记忆已清；重配对须用户发起）
    const observeEnd = Date.now() + 10_000;
    while (Date.now() < observeEnd) {
      const sb = await B.state();
      if (sb.sessions.some((x) => x.peer === pcFp)) {
        throw new Error('B 在 TrustBroken 后 10s 内会话复活——降级语义被自动重连破坏');
      }
      await sleep(1000);
    }
    return { detail: `toast=${toastSeen ? '已捕获' : '未捕获(截图目检)'}；双端会话断开；B 信任条目已对称删除；10s 观察窗无重连`, artifacts: [F('v-trust-broken-toast.png')] };
  });

  await step('收尾恢复夹具：L3 keepIdentity+seedTrust 互播 + 设备名还原 + connect 验证', async () => {
    await restorePcFixture();
    note(`夹具终态：huss_pc 信任=[huss_laptop]，huss_laptop 信任=[huss_pc]，设备名已还原，connect trusted=true`);
    return `L3 重置 + 互播 + connect trusted=true，设备名 huss_pc/huss_laptop`;
  });

  await step('收尾：证据 + test/end', async () => {
    const arts = [];
    for (const t of both) {
      try {
        await t.screenshot(join(runDir, `${t.name}-final.png`));
        arts.push(F(`${t.name}-final.png`));
      } catch (e) { note(`${t.name} 收尾截图失败（证据缺口，非断言）: ${e.message}`); }
    }
    const ev = await collectEvidence(both, runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    for (const t of both) {
      try { await t.beginTest(SCENARIO); await t.endTest('pass'); }
      catch (e) { note(`${t.name} test/end 失败（编排器报告为准）: ${e.message}`); }
    }
    return { detail: `证据 ${arts.length} 件落盘`, artifacts: arts };
  });
}

try {
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try {
    const ev = await collectEvidence(both, runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
    void ev;
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  try { await B.endTest('fail'); } catch { /* 尽力 */ }
  // 夹具兜底：PC 不能停在无信任态
  if (pcFp && laptopFp) {
    console.log(`[${SCENARIO}] 失败兜底：恢复 PC 信任夹具…`);
    try {
      await restorePcFixture();
      note('失败兜底夹具恢复完成');
    } catch (e3) { note(`失败兜底夹具恢复失败: ${e3.message}`); console.error(`[fixture] 兜底失败: ${e3.message}`); }
  }
}

const outcome = failureMsg ? 'fail' : (steps.length ? 'pass' : 'skip');
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '★ M3a T5：场景已落地、待 huss_laptop 恢复执行；设备不可达时条件跳过（outcome=skip, exit 0），run-all 注册表已接入',
      'TrustBroken 流向：移除方 remove_trusted → 断会话前 send_ctrl(TrustBroken) → 收通知方 core 删本端信任条目(双盲对称) → 清会话表+SessionDown → 壳层 toast「已移除对你的信任，连接已断开」',
      'wire 兼容：旧版对端收到 trust_broken 按"未知变体→解码失败→干净断连"收场，终态等价（本场景双端均新版，未覆盖旧端路径）',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
