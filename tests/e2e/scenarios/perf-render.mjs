// 场景：perf-render（P4 性能卡 T3 前端渲染 1000 卡,2026-09-09）
//
// PC 单机场景（不依赖手机/huss_laptop;PC 不可达条件跳过 exit 0）。
// 方案（无 debug seed 端点——最小方案）：临时预置 data/transfers.json 1000 条
// done 卡（新格式 {"cards":[{dto,removed:false}]}）→ L2 重启 → 断言：
//   a) 传输页折叠条计数 ≥1000
//   b) 首屏构造时长:点「历史」展开 → uiWait 命中末张卡（job_id 最大=最后渲染）
//      耗时 <2000ms（v-for 单次同步 patch,末卡挂载=全量挂载）
//   c) 交互 DOM 节点数（/api/ui/tree 可见交互元素口径）≥1000
// 滚动流畅度为人工目检项（webview 无程序化滚动注入面）——截图落证据,报告标注。
// 收尾无条件还原 transfers.json 备份并重启（含失败路径）。
//
// 已知语义（读 main.rs 实证,不影响本测）：内存态 1000 卡全量;transfers.json
// 落盘时非 removed 终态卡封顶 150（collect_persist_list）——只影响文件不影响 UI。
import { copyFileSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { pcDataDir } from '../lib/reset.mjs';
import { stopPcA, startPcA, waitReady } from '../lib/deploy.mjs';
import { regeneratePerfReport } from '../lib/perfreport.mjs';

const SCENARIO = 'perf-render';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);
const DATA_DIR = pcDataDir('huss_pc');
const TRANSFERS_JSON = join(DATA_DIR, 'transfers.json');
const SEED_COUNT = 1000;
const LAST_HEX = '00000000000003e8'; // job_id=1000 的 16 位 hex（map_cards 按 card_id 升序 → 末张）

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

/** 预置一条 done 卡（TransferDto serde 形态:job_id 是 16 位 hex 字符串） */
function seedCard(i, nowMs) {
  const started = nowMs - (SEED_COUNT - i) * 1000;
  return {
    dto: {
      job_id: i.toString(16).padStart(16, '0'),
      name: `perf-card-${String(i).padStart(4, '0')}.bin`,
      total: 1048576, done: 1048576,
      state: 'done', speed_bps: 0,
      peer: '11'.repeat(32),
      direction: 'push', local_role: 'source-push',
      started_at_ms: started, finished_at_ms: started + 1500,
      fail_reason: null,
      remote_done: 1048576, instant: false,
    },
    removed: false,
  };
}

/** 还原 transfers.json（备份存在→回灌;原不存在→删除）并重启 PC。失败路径也走。 */
async function restoreAndRestart(backupPath, hadBackup) {
  stopPcA();
  if (hadBackup) copyFileSync(backupPath, TRANSFERS_JSON);
  else rmSync(TRANSFERS_JSON, { force: true });
  startPcA(loadTargets());
  await waitReady(loadTargets().huss_pc, { label: 'huss_pc' });
  A.runId = null;
}

async function main() {
  let backupPath = null;
  let hadBackup = false;
  let restored = false;
  const result = { historyCount: 0, navMs: null, renderMs: null, domInteractiveNodes: 0, finishedAt: null };

  try {
    await step('环境握手 + 备份 transfers.json + 预置 1000 条 done 卡', async () => {
      const v = await A.version();
      if (v.bridgeReady !== true) throw new Error('huss_pc bridge 未就绪');
      versions.huss_pc = v;
      hadBackup = existsSync(TRANSFERS_JSON);
      if (hadBackup) {
        backupPath = join(runDir, 'transfers.json.bak');
        copyFileSync(TRANSFERS_JSON, backupPath);
      }
      stopPcA(); // 杀进程后改文件（规避 1s 脏落盘竞态）
      const nowMs = Date.now();
      const cards = [];
      for (let i = 1; i <= SEED_COUNT; i++) cards.push(seedCard(i, nowMs));
      writeFileSync(TRANSFERS_JSON, JSON.stringify({ cards }), 'utf8');
      startPcA(loadTargets());
      await waitReady(loadTargets().huss_pc, { label: 'huss_pc' });
      await A.beginTest(SCENARIO);
      await A.dismissResumePrompt();
      return `备份=${hadBackup ? '已留' : '原本不存在'};预置 ${SEED_COUNT} 条 done 卡 + 单机重启就绪`;
    });

    await step('导航传输页:折叠条计数 ≥1000（记录页面导航首屏）', async () => {
      const t0 = Date.now();
      await A.uiNavigate('/transfers');
      await A.uiWait('[testid=transfers-history-fold-btn]', 15_000);
      result.navMs = Date.now() - t0;
      const txt = await A.uiText('[testid=transfers-history-fold-btn]');
      const m = String(txt).match(/历史 \((\d+)\)/);
      if (!m) throw new Error(`折叠条文案异常: ${String(txt).slice(0, 80)}`);
      result.historyCount = Number(m[1]);
      if (result.historyCount < SEED_COUNT) {
        throw new Error(`历史卡数 ${result.historyCount} < 预置 ${SEED_COUNT}（启动重建丢卡?）`);
      }
      return `历史 (${result.historyCount}),导航首屏 ${result.navMs}ms`;
    });

    await step('首屏构造时长:展开 1000 卡 → 末卡 uiWait 命中 <2000ms', async () => {
      const t0 = Date.now();
      await A.uiClick('[testid=transfers-history-fold-btn]');
      await A.uiWait(`[testid=transfer-item-${LAST_HEX}]`, 30_000);
      result.renderMs = Date.now() - t0;
      if (result.renderMs >= 2000) throw new Error(`首屏构造 ${result.renderMs}ms ≥ 2000ms 预算`);
      return `1000 卡展开首屏 ${result.renderMs}ms（点击→末卡 job_id=${LAST_HEX} 命中）`;
    });

    await step('DOM 节点计数 + 截图目检证据', async () => {
      const tree = await A.uiTree();
      result.domInteractiveNodes = Array.isArray(tree) ? tree.length : 0;
      if (result.domInteractiveNodes < SEED_COUNT) {
        throw new Error(`交互 DOM 节点 ${result.domInteractiveNodes} < ${SEED_COUNT}（每卡至少 1 个交互元素）`);
      }
      const shot = join(runDir, 'v-pc-1000cards.png');
      await A.screenshot(shot);
      // 滚动流畅度:人工目检项——webview 无程序化滚动注入面,自动化不可达,
      // 按验收惯例标注留给人工复核（证据=上图）
      note('滚动流畅度=人工目检项:自动化无滚动注入面,证据截图 v-pc-1000cards.png 供目检');
      return { detail: `交互节点 ${result.domInteractiveNodes}（可见交互元素口径,非全 DOM 树）`, artifacts: [F('v-pc-1000cards.png')] };
    });

    await step('result.json + 基线报告再生 + 收尾还原', async () => {
      result.finishedAt = new Date().toISOString();
      writeFileSync(join(runDir, 'result.json'), JSON.stringify(result, null, 2));
      const doc = regeneratePerfReport('perf-render', result);
      console.log(`  基线报告: ${doc}`);
      await A.endTest('pass');
      await restoreAndRestart(backupPath, hadBackup);
      restored = true;
      return { detail: `transfers.json 已还原（备份=${hadBackup}）+ 报告再生`, artifacts: [F('result.json')] };
    });
  } finally {
    // 失败路径兜底还原（成功路径上一步已还原,restored 标记防重复重启）
    if (hadBackup !== null && !restored) {
      try { await restoreAndRestart(backupPath, hadBackup); restored = true; } catch (e) { note(`兜底还原失败: ${e.message}`); }
    }
  }
}

try {
  console.log(`[${SCENARIO}] 开始  runId=${runId}（PC 单机,1000 卡渲染)`);
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
      '方案:transfers.json 预置 1000 条 done 卡 + 单机重启（无 debug seed 端点,最小方案）',
      '首屏口径:点「历史」展开 → uiWait 命中末张卡（v-for 同步 patch,末卡挂载=全量挂载）',
      'DOM 计数口径:/api/ui/tree 可见交互元素（INTERACTIVE_SELECTOR）,非全 DOM 树',
      '滚动流畅度=人工目检项（webview 无程序化滚动注入面）',
      '已知语义:transfers.json 落盘时非 removed 终态卡封顶 150（内存态/UI 不受影响）',
      '收尾:transfers.json 备份无条件还原（含失败路径）',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
