// 场景：routing-probe（M3b T4 探测选路·通道记录端到端，计划卡 2026-09-08-m3b-probe-score T4）
//
// ★★ 双机条件跳过场景：任一 PC bridge 未就绪 → SKIP 退出（exit 0 + 报告 outcome=skip），
//    已注册 run-all（注册表照常通过）。部署前置：node lib/deploy.mjs（list_channels 为
//    本场景新增只读命令，旧构建 403 INVOKE_NOT_ALLOWED = 未部署新构建的信号）。★★
//
// 验收路径（PC 可验部分 = 通道记录/观测面端到端；评分/退化切换的双地址逻辑不在本场景）：
//   0) 双 PC 不可达（任一 bridge 未就绪）→ SKIP（条件跳过设计）。
//   1) 环境握手 + 会话在位（fixture trusted+connected；不在则 connect 编排注入——
//      connect 是白名单内 Mutating 命令，语义注入与 M6 同口径）。
//   2) A 侧通道记录断言（invoke list_channels，M3b 新增 ReadOnly 白名单命令）：
//      A 的通道表含 huss_laptop 的记录，且 SessionUp 触发的全量探测已实采——
//      rtt_ms 非空、est_bps>0、近10次丢包 loss_rate=0、current=true（当前通道指针）、
//      score_ready=true（评分选路数据齐备）。
//   3) B 侧对称断言：SessionUp 事件泵双端对称（PC main.rs 对 SessionUp 调
//      probe::on_session_up），B 的通道表同样含 huss_pc 的已评分记录。
//   4) 第二地址模拟段：**记 SKIP 注记非断言**——测试白名单无 connect_pinned（不允许
//      任意地址建连，红线）；辅助 IP 需要改测试机 NIC 配置（不可行）。多地址评分选路
//      /退化切换已由 core 单测 + src-tauri 环回测试（probe.rs 127.0.0.1 双 listener）
//      覆盖；本场景锚定"单通道现状不回归 + 观测面真实数据"。
//   5) 收尾：双端截图 + 证据 + test/end（本场景无破坏性操作，无夹具恢复需求）。
//
// 用法：node scenarios/routing-probe.mjs（前置：node lib/deploy.mjs 已部署双端）
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';

const SCENARIO = 'routing-probe';
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

/** 可达探测（短超时）：对端休眠时 fetch 快速失败/超时 → 调用方走 SKIP 出口 */
async function reachable(t, timeoutMs = 8000) {
  try {
    const v = await t.ok('/api/version', { timeoutMs });
    return v?.bridgeReady === true;
  } catch { return false; }
}

/** 轮询某设备的通道记录直至 pred 满足（list_channels 无快照源，invoke 级轮询）。
 * 返回该设备的通道记录数组（末次观测）。 */
async function pollChannels(t, fp, pred, { timeoutMs = 60_000, intervalMs = 1000, what = '通道记录' } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last = [];
  for (;;) {
    last = await t.invoke('list_channels');
    const mine = last.filter((c) => c.fingerprint === fp);
    if (pred(mine, last)) return mine;
    if (Date.now() >= deadline) {
      throw new Error(`${t.name} 轮询超时（${timeoutMs}ms）: ${what}；末次观测=${JSON.stringify(last)}`);
    }
    await sleep(intervalMs);
  }
}

/** 单侧通道记录断言：恰一条、当前通道、探测数据齐备、稳定窗干净 */
function assertSingleScoredChannel(t, peerFp, peerName, records) {
  if (records.length !== 1) {
    throw new Error(`${t.name} 通道表应恰含 ${peerName} 一条记录（单通道现状锚），实际 ${records.length}: ${JSON.stringify(records)}`);
  }
  const c = records[0];
  if (c.current !== true) throw new Error(`${t.name} 记录 current 应为 true: ${JSON.stringify(c)}`);
  if (c.rtt_ms == null) throw new Error(`${t.name} rtt_ms 未采集（SessionUp 全量探测未运行）: ${JSON.stringify(c)}`);
  if (!(c.est_bps > 0)) throw new Error(`${t.name} est_bps 应 >0: ${JSON.stringify(c)}`);
  if (c.score_ready !== true) throw new Error(`${t.name} score_ready 应为 true: ${JSON.stringify(c)}`);
  if (c.loss_rate !== 0) throw new Error(`${t.name} 局域网稳定窗丢包应 0: ${JSON.stringify(c)}`);
  if (c.probe_disabled !== false) throw new Error(`${t.name} 健康通道不应被探测拉黑: ${JSON.stringify(c)}`);
  // via_relay:仅中继关闭时强断言 false（relay-path 场景另测经中继记录）
  return c;
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口：任一 PC 不可达 → SKIP（不等待设备恢复）----
  const aUp = await reachable(A);
  const bUp = aUp ? await reachable(B) : false;
  if (!aUp || !bUp) {
    const msg = `SKIP：${!aUp ? 'huss_pc' : 'huss_laptop'} 不可达（bridge 未就绪）——本场景待设备恢复后执行`;
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
          '执行前置：node lib/deploy.mjs 重建双端（list_channels 为 M3b 新增只读命令，旧构建 403）',
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
    // 部署自检：新命令在白名单内（旧构建 403 INVOKE_NOT_ALLOWED = 未部署）
    await A.invoke('list_channels');
    await B.invoke('list_channels');
    return `huss_pc v${versions.huss_pc.appVersion} fp=${pcFp.slice(0, 8)}…，huss_laptop v${versions.huss_laptop.appVersion} fp=${laptopFp.slice(0, 8)}…（list_channels 在白名单=新构建已部署）`;
  });

  await step('双 PC test/begin', async () => {
    const a = await A.beginTest(SCENARIO);
    const b = await B.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…`;
  });

  await step('会话在位：fixture 已互信；无会话则 A connect 注入（白名单 Mutating 同 M6 口径）', async () => {
    const up = async (t, peerFp) =>
      (await t.state()).sessions.some((x) => x.peer === peerFp && x.trusted);
    if (!(await up(A, laptopFp))) {
      await A.invoke('connect', { fingerprint: laptopFp });
      await A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc 会话建立' });
      await B.pollUntil((s) => s.sessions.some((x) => x.peer === pcFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop 会话建立' });
      return 'fixture 无在位会话 → connect 注入并确认双端 trusted';
    }
    return `会话在位（A→B trusted=${await up(A, laptopFp)}）——连接记忆制下无重复建连`;
  });

  await step('A 侧通道记录断言：SessionUp 全量探测实采（rtt/est_bps/丢包/当前指针/评分就绪）', async () => {
    const relayEnabled = (await A.invoke('get_settings')).relay_enabled;
    const records = await pollChannels(A, laptopFp, (mine) => mine.length > 0 && mine[0].score_ready === true,
      { what: `huss_pc 表内 huss_laptop 的已评分通道记录` });
    const c = assertSingleScoredChannel(A, laptopFp, 'huss_laptop', records);
    if (!relayEnabled && c.via_relay) {
      throw new Error(`huss_pc 中继未启用但记录标记 via_relay=true: ${JSON.stringify(c)}`);
    }
    await A.screenshot(join(runDir, 'v-a-channel-live.png'));
    return `addr=${c.addr} rtt=${c.rtt_ms}ms est=${(c.est_bps / 1e6).toFixed(1)}Mbps loss=${c.loss_rate} via_relay=${c.via_relay}(relay_enabled=${relayEnabled}) age=${c.age_secs}s`;
  });

  await step('B 侧对称断言：SessionUp 事件泵双端对称，B 表同样含 huss_pc 已评分记录', async () => {
    const records = await pollChannels(B, pcFp, (mine) => mine.length > 0 && mine[0].score_ready === true,
      { what: `huss_laptop 表内 huss_pc 的已评分通道记录` });
    const c = assertSingleScoredChannel(B, pcFp, 'huss_pc', records);
    await B.screenshot(join(runDir, 'v-b-channel-live.png'));
    return `addr=${c.addr} rtt=${c.rtt_ms}ms est=${(c.est_bps / 1e6).toFixed(1)}Mbps loss=${c.loss_rate} current=${c.current}`;
  });

  await step('第二地址模拟：不可行 → SKIP 注记（评分/切换由单测+环回覆盖）', async () => {
    const msg = '测试白名单无 connect_pinned（不允许对任意地址建连）；辅助 IP 需改测试机 NIC（不可行）'
      + '——多地址评分选路/退化切换由 core 单测 + src-tauri 环回测试（probe.rs 127.0.0.1 双 listener）覆盖；'
      + '本场景锚定单通道现状不回归 + 观测面真实数据';
    note(`SKIP 注记：${msg}`);
    return msg;
  });

  await step('收尾：证据 + test/end（只读场景，无夹具恢复需求）', async () => {
    const arts = [];
    for (const t of both) {
      try {
        await t.screenshotSoft(join(runDir, `${t.name}-final.png`));
        arts.push(F(`${t.name}-final.png`));
      } catch (e) { note(`${t.name} 收尾截图失败（证据缺口，非断言）: ${e.message}`); }
    }
    const ev = await collectEvidence(both, runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    for (const t of both) {
      try { await t.endTest('pass'); }
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
    await collectEvidence(both, runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  try { await B.endTest('fail'); } catch { /* 尽力 */ }
}

const outcome = failureMsg ? 'fail' : (steps.length ? 'pass' : 'skip');
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '★ M3b T4：通道记录/观测面端到端（list_channels 新只读命令为断言数据源）；任一 PC 不可达自动条件跳过（outcome=skip, exit 0），run-all 已注册',
      '探测链路：connect/会话建立 → SessionUp 事件 → probe::on_session_up 登记 ChannelTable + 阶梯全量探测（RTT=Ping×3 中位；带宽=64KB→512KB→4MB 出站中位段速率）',
      '观测面：list_channels 返回每设备每通道 {fingerprint,addr,via_relay,rtt_ms,est_bps,loss_rate,current,score_ready,probe_disabled,age_secs}；数据=AppState.channels 内存态（core::routing::ChannelTable，不持久化）',
      '范围边界：多地址评分选路/退化切换不在本场景（白名单无 connect_pinned、辅助 IP 不可行）——core 单测 + src-tauri 环回测试覆盖；本场景=单通道现状不回归锚',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
