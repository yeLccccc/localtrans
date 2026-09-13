// 场景：perf-idle（P4 性能卡 T4 空闲足迹,2026-09-09）
//
// PC 单机场景（不依赖手机/huss_laptop;PC 不可达条件跳过 exit 0）。
// 流程：确认无活动传输 → 无传输静置 5 分钟,每 30s 采样 PC 进程 CPU
//（CIM PercentProcessorTime,单核口径:100%=1 核）→ 断言均值 <2%。
// 单点 >5% 记为"周期扫描疑点"写进报告备注（发现周期扫描异常的观测面）;
// 起止各记一次进程工作集内存（足迹）。
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { stopPcA, startPcA, waitReady } from '../lib/deploy.mjs';
import { regeneratePerfReport } from '../lib/perfreport.mjs';
import { sampleProcessCpu, processWorkingSetBytes } from '../lib/winmetrics.mjs';

const SCENARIO = 'perf-idle';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);

const SAMPLES = 10;        // 10 × 30s = 5 分钟
const INTERVAL_MS = 30_000;
const MEAN_BUDGET = 2;     // 断言:均值 <2%（单核口径）
const SPIKE_MARK = 5;      // 单点 >5% 记疑点

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let skipReason = null;

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

const baseOf = (cfg) => `http://${cfg.host}:${cfg.port}`;
const reachable = (base) => fetch(base + '/api/health', { signal: AbortSignal.timeout(4000) }).then((r) => r.ok).catch(() => false);

const pcUp = await reachable(baseOf(targets.huss_pc));
if (!pcUp) skipReason = 'huss_pc test-api 不可达（先跑 node lib/deploy.mjs --skip-huss_laptop）';
if (skipReason) {
  console.log(`[${SCENARIO}] SKIP: ${skipReason}`);
  const reportFile = writeReport({
    runId, scenario: SCENARIO, steps: [], outcome: 'skip',
    env: { runDir, startedAt: startedAt.toISOString(), finishedAt: new Date().toISOString(), notes: [skipReason] },
  });
  console.log(`[${SCENARIO}] 报告: ${reportFile}`);
  process.exit(0);
}

const ACTIVE_STATES = ['active', 'pending', 'cancelling', 'paused'];

async function main() {
  await step('环境握手 + 清场:无活动传输 + 单机重启干净起步', async () => {
    const v = await A.version();
    if (v.bridgeReady !== true) throw new Error('huss_pc bridge 未就绪');
    versions.huss_pc = v;
    // 单机 L2（stopPcA/startPcA,不依赖 huss_laptop）——清掉链式场景可能留的活动引擎
    stopPcA();
    startPcA(loadTargets());
    await waitReady(loadTargets().huss_pc, { label: 'huss_pc' });
    await A.beginTest(SCENARIO);
    await A.dismissResumePrompt();
    await reset(1, [A]); // 终态卡 view 级清理
    const s = await A.state();
    const actives = (s.transfers || []).filter((t) => ACTIVE_STATES.includes(t.state));
    if (actives.length > 0) {
      throw new Error('应有 0 个活动传输: ' + JSON.stringify(actives.map((t) => ({ id: t.id, state: t.state }))));
    }
    return '重启就绪 + 0 活动传输 ✓';
  });

  await step(`空闲 5 分钟:每 30s 采样 PC 进程 CPU（${SAMPLES} 拍,单核口径）`, async () => {
    // 停在设备页（应用空闲主态,无传输进行）
    await A.uiNavigate('/devices');
    const memStart = await processWorkingSetBytes();
    const samples = [];
    for (let i = 0; i < SAMPLES; i++) {
      await sleep(INTERVAL_MS);
      const cpu = await sampleProcessCpu();
      samples.push({ t: Date.now(), cpuPercent: cpu });
      console.log(`    ${(i + 1) * 30}s: CPU ${cpu === null ? '采样失败' : cpu.toFixed(1) + '%'}`);
    }
    const memEnd = await processWorkingSetBytes();
    const vals = samples.map((s) => s.cpuPercent).filter((v) => v !== null);
    if (vals.length < SAMPLES / 2) throw new Error(`有效采样 ${vals.length}/${SAMPLES} 过少——CIM 采样链路异常`);
    const mean = vals.reduce((a, b) => a + b, 0) / vals.length;
    const max = Math.max(...vals);
    const spikes = vals.filter((v) => v > SPIKE_MARK);
    // 静置期间不得自发出现传输
    const s2 = await A.state();
    const actives = (s2.transfers || []).filter((t) => ACTIVE_STATES.includes(t.state));
    if (actives.length > 0) throw new Error('静置期间出现活动传输（异常自发传输）');

    const result = {
      samples, sampleCount: vals.length,
      meanPercent: Number(mean.toFixed(3)),
      maxPercent: max,
      spikeCount: spikes.length,
      memStartBytes: memStart, memEndBytes: memEnd,
      note: spikes.length > 0
        ? `发现 ${spikes.length} 个 >${SPIKE_MARK}% 单点（峰值 ${max.toFixed(1)}%）——周期扫描疑点,结合 tracing 日志排查`
        : `无 >${SPIKE_MARK}% 单点,未见周期扫描异常`,
      finishedAt: new Date().toISOString(),
    };
    if (mean >= MEAN_BUDGET) {
      throw new Error(`空闲 CPU 均值 ${mean.toFixed(2)}% ≥ ${MEAN_BUDGET}% 预算（单核口径）`);
    }
    writeFileSync(join(runDir, 'result.json'), JSON.stringify(result, null, 2));
    const doc = regeneratePerfReport('perf-idle', result);
    console.log(`  基线报告: ${doc}`);
    if (spikes.length > 0) note(result.note);
    return {
      detail: `均值 ${mean.toFixed(2)}% 峰值 ${max.toFixed(1)}% 内存 ${(memStart / 1048576).toFixed(0)}→${memEnd === null ? '—' : (memEnd / 1048576).toFixed(0)}MB;${result.note}`,
      artifacts: [F('result.json')],
    };
  });

  await step('证据落盘 + test/end', async () => {
    const arts = [];
    const ev = await collectEvidence([A], runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    await A.endTest('pass');
    return { detail: `证据 ${arts.length} 件落盘`, artifacts: arts };
  });
}

try {
  console.log(`[${SCENARIO}] 开始  runId=${runId}（PC 单机,约 6 分钟）`);
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
}

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      'CPU 口径:CIM PercentProcessorTime,单核口径(100%=1 核);均值断言 <2%',
      '单点 >5% 记周期扫描疑点(不致死,写报告备注)',
      'PC 单机场景;静置期间出现自发活动传输即 FAIL',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
