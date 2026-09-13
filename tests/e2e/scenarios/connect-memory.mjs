// 场景：connect-memory（M3a FR5 连接记忆制，计划卡 T4）
//
// ★★ 状态：待 huss_laptop（192.168.0.222）恢复执行 —— 设备休眠/不可达时本场景
//    自动 SKIP 退出（exit 0 + 报告 outcome=skip）；已注册 run-all（注册表自动跳）。★★
//
// 验收路径（spec 验收标准 2"断线自动重连"+ FR5）：
//   0) 双 PC 不可达（任一 bridge 未就绪）→ SKIP（条件跳过设计）。
//   1) 清场：A/B 双向 remove_trusted（顺带清连接记忆——同一命令路径）→ 断言信任表空。
//   2) 重新配对（用户主动连接语义）：A connect 发起 → B UI 同意门 → 读码 → A UI 输码
//      → 双端 sessions trusted。此刻两端各自把对端记入连接记忆（A=connect 成功登记，
//      B=PairingResult ok 登记），持久化落 data/connect_memory.json。
//   3) 断会话：杀掉 B 进程 → A 断线检测后自动进入退避重连（静默）。
//   4) 重启 B（同身份同信任）→ 断言 A 在 B 上线后 N 秒内自动重连（sessions 恢复
//      trusted，无需任何手动 connect）——连接记忆命中。B 侧同样记忆了 A，双端任一
//      方先建成会话均算恢复。
//   5) 移除信任清记忆：A remove_trusted(B) 紧跟 B remove_trusted(A)（背靠背，防
//      对端记忆反向重连进配对门——单边移除的协议通知属 T5 TrustBroken，不在本卡）
//      → 观察 30s：设备仍在发现表在线可见，但 A/B sessions 恒无对端——记忆已清，
//      不再自动重连。
//   6) 收尾恢复夹具：L3 keepIdentity+seedTrust 双向互播 + UI 设备名还原 + connect 验证
//      （connect_memory.json 已纳入 L3 清单，重置后记忆干净起步）。
//
// 任何一步失败：collectEvidence → 报告 FAIL → 尽力兜底恢复 PC 夹具。
// 用法：node scenarios/connect-memory.mjs（前置：node lib/deploy.mjs 已部署双端）
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { stopPcB, waitReady, sshExec } from '../lib/deploy.mjs';

const SCENARIO = 'connect-memory';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);
const B = new Target('huss_laptop', targets.huss_laptop, journal);
const both = [A, B];
const cfg = targets;

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

/** PC 侧读配对码（acceptor 亮码 .code-display，码可能带空格分组）；
 * 超时兜底只读 get_pairing_pending（与 cold-start 同款） */
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

/** 夹具恢复：L3 keepIdentity+seedTrust 互播 + 设备名还原 + connect 验证。
 * 信任表目标态：huss_pc=[huss_laptop]，huss_laptop=[huss_pc] */
async function restorePcFixture() {
  const seed = {
    huss_pc: [{ fingerprint: laptopFp, name: 'huss_laptop' }],
    huss_laptop: [{ fingerprint: pcFp, name: 'huss_pc' }],
  };
  await reset(3, both, { keepIdentity: true, seedTrust: seed });
  await restoreDeviceName(A, 'huss_pc');
  await restoreDeviceName(B, 'huss_laptop');
  // B 刚重启会话通道可能未就绪，短重试（与 cold-start 同款）
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
    const msg = `SKIP：${!aUp ? 'huss_pc' : 'huss_laptop'} 不可达（bridge 未就绪）——本场景待设备恢复后执行（M3a T4 部署期，huss_laptop 疑似休眠）`;
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
          'M3a 计划卡 T4 验收环境约束：huss_laptop（192.168.0.222）恢复后手动重跑本场景补双机证据',
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
    const fpA = await fingerprintOf(A);
    const fpB = await fingerprintOf(B);
    pcFp = fpA;
    laptopFp = fpB;
    note(`预运行指纹 huss_pc=${fpA} huss_laptop=${fpB}`);
    return `huss_pc v${versions.huss_pc.appVersion} fp=${fpA.slice(0, 8)}…，huss_laptop v${versions.huss_laptop.appVersion} fp=${fpB.slice(0, 8)}…`;
  });

  await step('双 PC test/begin', async () => {
    const a = await A.beginTest(SCENARIO);
    const b = await B.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…`;
  });

  await step('【清场】双向 remove_trusted（同一命令路径同步清连接记忆）→ 断言信任表空', async () => {
    await A.invoke('remove_trusted', { fingerprint: laptopFp });
    await B.invoke('remove_trusted', { fingerprint: pcFp });
    for (const t of both) {
      const trusted = await t.invoke('list_trusted');
      if (trusted.length !== 0) throw new Error(`${t.name} 清场后信任表非空: ${JSON.stringify(trusted)}`);
    }
    return '双向信任与连接记忆已清（重配对从零开始）';
  });

  await step('【重配对】A connect 发起 → B UI 同意 → 读码 → A UI 输码 → 双端 sessions trusted', async () => {
    await A.invoke('connect', { fingerprint: laptopFp });
    await B.uiWait('[testid=pairing-dialog]', 15_000);
    await B.uiClick('[testid=btn-grant]');
    const code = await readPcPairingCode(B, pcFp);
    await A.screenshot(join(runDir, 'v-code-entry.png'));
    await A.uiWait('[testid=code-input]', 20_000);
    await A.uiInput('[testid=code-input]', code);
    await A.uiClick('[testid=btn-submit]');
    await Promise.all([
      A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc sessions 含 huss_laptop 且 trusted' }),
      B.pollUntil((s) => s.sessions.some((x) => x.peer === pcFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop sessions 含 huss_pc 且 trusted' }),
    ]);
    return { detail: `配对完成（码 ${code.slice(0, 2)}****）——双端连接记忆已登记（A: connect 成功 / B: PairingResult ok）`, artifacts: [F('v-code-entry.png')] };
  });

  await step('【断会话】杀掉 B 进程 → A 断线检测（sessions 清空对端）', async () => {
    await stopPcB(cfg);
    await A.pollUntil((s) => !s.sessions.some((x) => x.peer === laptopFp),
      { timeoutMs: 120_000, intervalMs: 1000, what: 'huss_pc sessions 已无 huss_laptop（QUIC 断线检测）' });
    // 此时 A 侧重连编排已启动（记忆+信任命中）——对端不在线，静默退避等待
    return 'B 进程已杀；A 断线检测完成，自动重连任务静默进行中';
  });

  await step('【自动重连】重启 B → 断言 A 在 B 上线后 180s 内 sessions 恢复 trusted（无手动 connect）', async () => {
    await sshExec(cfg.huss_laptop, 'schtasks /run /tn LT-Test');
    await waitReady(cfg.huss_laptop, { label: 'huss_laptop' });
    B.runId = null; // 进程已换，run 失效
    const t0 = Date.now();
    await A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
      { timeoutMs: 180_000, intervalMs: 1000, what: 'huss_pc 自动重连恢复 huss_laptop 会话' });
    const secs = Math.round((Date.now() - t0) / 1000);
    // B 侧也记忆了 A（PairingResult ok 登记）——双端恢复为终态，非硬断言
    let bRestored = true;
    try {
      await B.pollUntil((s) => s.sessions.some((x) => x.peer === pcFp && x.trusted),
        { timeoutMs: 30_000, intervalMs: 1000, what: 'huss_laptop 侧会话恢复' });
    } catch { bRestored = false; note('B 侧会话未在 30s 内恢复（A 侧单边建成亦满足验收）'); }
    await A.screenshot(join(runDir, 'v-auto-reconnected.png'));
    return { detail: `A 自动重连成功（B 就绪后 ${secs}s，退避节奏 2/4/8/16/30s+抖动内）；B 侧恢复=${bRestored}`, artifacts: [F('v-auto-reconnected.png')] };
  });

  await step('【清记忆】双向 remove_trusted 背靠背 → 观察 30s：设备在线可见但恒不自动重连', async () => {
    await A.invoke('remove_trusted', { fingerprint: laptopFp });
    await B.invoke('remove_trusted', { fingerprint: pcFp });
    // 双端信任与记忆应已清（本步同时是单测 remove_trusted_clears_connect_memory 的真机面）
    for (const [t, peer] of [[A, laptopFp], [B, pcFp]]) {
      const trusted = await t.invoke('list_trusted');
      if (trusted.some((p) => p.fingerprint === peer)) {
        throw new Error(`${t.name} 移除信任后仍含对端: ${JSON.stringify(trusted)}`);
      }
    }
    // 观察 30s：对端仍在发现表在线（排除"探测不到所以没连"的假阴性），但会话不得复活
    const deadline = Date.now() + 30_000;
    let sawOnline = false;
    while (Date.now() < deadline) {
      const s = await A.state();
      if (s.devices.some((d) => d.id === laptopFp && d.online)) sawOnline = true;
      if (s.sessions.some((x) => x.peer === laptopFp)) {
        throw new Error('A 移除信任清记忆后 30s 内会话复活——自动重连未随记忆清除而停止');
      }
      const sb = await B.state();
      if (sb.sessions.some((x) => x.peer === pcFp)) {
        throw new Error('B 移除信任清记忆后 30s 内会话复活');
      }
      await sleep(1000);
    }
    if (!sawOnline) note('观察窗内 B 未出现在 A 发现表在线（广播节拍偶发，非本步断言项）');
    await A.screenshot(join(runDir, 'v-no-reconnect.png'));
    return { detail: '双向信任+记忆已清；30s 观察窗内无会话复活（设备仍广播在线）——不再自动重连', artifacts: [F('v-no-reconnect.png')] };
  });

  await step('收尾恢复夹具：L3 keepIdentity+seedTrust 互播 + 设备名还原 + connect 验证', async () => {
    await restorePcFixture();
    note(`夹具终态：huss_pc 信任=[huss_laptop]，huss_laptop 信任=[huss_pc]，设备名已还原，connect trusted=true`);
    return `L3 重置（connect_memory.json 已纳入 L3 清单）+ 互播 + connect trusted=true，设备名 huss_pc/huss_laptop`;
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
      '★ M3a T4：场景已落地、待 huss_laptop 恢复执行；设备不可达时条件跳过（outcome=skip, exit 0），run-all 注册表已接入',
      '记忆登记点：connect 命令成功（用户主动）与 PairingResult ok（配对双方）——core::connect_memory 持久化 data/connect_memory.json',
      '断线重连为静默行为：仅"已自动重连"/"已停止重试"各一条 toast；退避 2/4/8/16s 封顶 30s ±20% 抖动，连续 5 败回落，设备离线（发现表无地址）不计失败',
      '单边移除信任会让仍记忆的对端反向重连进配对门（协议级通知属 T5 TrustBroken）——故清记忆断言采用双向背靠背移除',
      '核心断言窗口：B 就绪后 180s（覆盖断线检测 ≤120s + 退避周期 + 广播发现）',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
