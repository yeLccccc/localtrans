// 场景：parts-deleted-integrity（V1 计划卡 T2——桌面核对单第 7 条）
//
// ★★ PC 单机可跑：不依赖 huss_laptop/huss_phone；huss_pc bridge 未就绪时
//    条件 SKIP 退出（exit 0 + 报告 outcome=skip）。已注册 run-all。★★
//
// 验收路径（2026-08-30 传输域重构 Task 12 核对单 #7）：
//   "手动删 parts 目录后重启:对应卡片显示 failed("数据丢失/完整性存疑")而非可续传"
//
// 产品行为锚点（crates/localtrans-core/src/transfer/mod.rs + src-tauri main.rs rebuild_cards）：
//   "收齐却没 finalize = finalize 中途被打断，数据完整性存疑"——位图全真的 parts 目录
//   在启动重建时：① 建卡 state=failed、fail_reason="数据完整性存疑,建议重新拉取"；
//   ② gc_stale_parts 直接删除该 parts 目录（=人工资源管理器核对"parts 目录消失"）。
//   单机等价法：种子位图全真 manifest（≈ finalize 前一瞬被强杀的磁盘现场），
//   真实 taskkill → 重启，断言 failed 卡 + parts 目录已被清 + 三个"非可续传"面。
//
// 用法：node scenarios/parts-deleted-integrity.mjs（前置：node lib/deploy.mjs --skip-huss_laptop）
import { mkdirSync, existsSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot, repoRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { startPcA, stopPcA, waitReady } from '../lib/deploy.mjs';

const SCENARIO = 'parts-deleted-integrity';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);
const note = (m) => journal.append({ kind: 'note', message: m });

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let downloadDir = null;
let jobSeed = null;

const F = (n) => `reports/${runId}/${n}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const CHUNK = 4 * 1024 * 1024; // protocol::CHUNK_SIZE
const N_CHUNKS = 6; // 全部收齐（位图全真）

async function reachable(t, timeoutMs = 8000) {
  try {
    const v = await t.ok('/api/version', { timeoutMs });
    return v?.bridgeReady === true;
  } catch { return false; }
}

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

/** 种子"收齐未 finalize"现场：位图全真 manifest（gc 判定与卡片状态的唯一依据） */
function seedCompleteParts() {
  const displayName = `v1-integrity-${rand}.bin`;
  const job = 0x5200000000 + (Date.now() % 0xfffff0); // 引擎 ID 段(0x4000/0x8000)之外，安全整数
  const jobHex = job.toString(16).padStart(16, '0'); // 与产品 job_dir/snapshot 的 {:016x} 同形
  const dir = join(downloadDir, '.localtrans-parts', jobHex);
  mkdirSync(dir, { recursive: true });
  const manifest = {
    file_name: displayName,
    total_size: N_CHUNKS * CHUNK,
    chunk_hashes: Array.from({ length: N_CHUNKS }, (_, i) => ('f' + i).repeat(32)),
    received: Array(N_CHUNKS).fill(true), // 位图全真：差一步 finalize
    peer: jobSeed.fpSelf,
    meta: {
      direction: 'pull',
      local_role: 'destination',
      display_name: displayName,
      peer_hex: jobSeed.fpSelf,
      created_at_ms: Date.now(),
      finished_at_ms: null,
      fail_reason: null,
      source_path: null,
      batch_label: null,
    },
  };
  writeFileSync(join(dir, 'manifest.json'), JSON.stringify(manifest, null, 2), 'utf8');
  jobSeed = { ...jobSeed, job: Number(job), jobHex, jobDir: dir, displayName, createdAtMs: manifest.meta.created_at_ms };
  note(`种子位图全真现场 ${jobHex} → ${dir}`);
  return jobSeed;
}

/** 收尾：transfers.json 种子条目清理（parts 目录此时已被产品 GC 删除）+ 重启还原 */
async function cleanupSeed() {
  const tj = join(repoRoot(), 'target', 'release', 'data', 'transfers.json');
  if (jobSeed && existsSync(tj)) {
    try {
      const parsed = JSON.parse(readFileSync(tj, 'utf8'));
      const before = (parsed.cards || []).length;
      // transfers.json dto.job_id 是 16 位 hex 字符串（u64_hex_string 序列化）
      parsed.cards = (parsed.cards || []).filter((c) => String(c.dto?.job_id ?? '').toLowerCase() !== jobSeed.jobHex);
      if (parsed.cards.length !== before) writeFileSync(tj, JSON.stringify(parsed, null, 2), 'utf8');
      note(`transfers.json 清理：移除 ${before - parsed.cards.length} 条种子条目`);
    } catch (e) { note(`transfers.json 清理跳过（解析失败，留引擎自愈）: ${e.message}`); }
  }
  if (jobSeed?.jobDir && existsSync(jobSeed.jobDir)) {
    rmSync(jobSeed.jobDir, { recursive: true, force: true });
    note('（异常路径）种子 parts 目录仍在，已手动清除');
  }
}

async function restartPc(label) {
  stopPcA();
  startPcA(targets);
  await waitReady(targets.huss_pc, { label, timeoutMs: 120_000 });
  A.runId = null;
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口 ----
  if (!(await reachable(A))) {
    const msg = 'SKIP：huss_pc 不可达（bridge 未就绪）——先跑 node lib/deploy.mjs --skip-huss_laptop';
    console.log(`\n[${SCENARIO}] ${msg}`);
    note(msg);
    writeReport({
      runId, scenario: SCENARIO, steps: [{ name: '可达探测', status: 'SKIP', detail: msg, durationMs: 0 }],
      outcome: 'skip',
      env: {
        runDir, startedAt: startedAt.toISOString(), finishedAt: new Date().toISOString(),
        durationMs: Date.now() - startedAt, versions,
        notes: ['条件跳过设计：huss_pc bridge 未就绪即 SKIP 退出（exit 0），run-all 注册表照常通过'],
      },
    });
    return;
  }

  await step('环境握手：huss_pc bridge 就绪 + download_dir 定位', async () => {
    const v = await A.version();
    if (v.bridgeReady !== true) throw new Error('bridge 未就绪');
    versions.huss_pc = v;
    const settings = await A.invoke('get_settings');
    downloadDir = settings.download_dir;
    const fpSelf = (await A.invoke('get_device_fingerprint')).fingerprint_hex;
    jobSeed = { fpSelf };
    if (!existsSync(downloadDir)) throw new Error(`download_dir 不存在: ${downloadDir}`);
    return `v${v.appVersion}，download_dir=${downloadDir}`;
  });

  await step(`种子"收齐未 finalize"现场：位图全真 ${N_CHUNKS}/${N_CHUNKS} 块（运行中应用不感知）`, () => {
    const s = seedCompleteParts();
    return `parts=${s.jobDir}`;
  });

  await step('真实强杀：taskkill /F localtrans.exe（等同 finalize 前被杀）', async () => {
    stopPcA();
    if (!(await reachable(A, 1500))) return '进程已强杀，bridge 无响应（符合预期）';
    throw new Error('taskkill 后 bridge 仍可达——强杀未生效');
  });

  await step('重启 → 断言卡 failed（完整性存疑）而非 interrupted', async () => {
    startPcA(targets); // 强杀后进程已不在——脱离编排会话拉起
    await waitReady(targets.huss_pc, { label: 'huss_pc', timeoutMs: 120_000 });
    A.runId = null;
    await A.beginTest(SCENARIO);
    const hit = await A.pollUntil(
      (s) => s.transfers.find((t) => t.id === jobSeed.jobHex) ?? false,
      { timeoutMs: 30_000, intervalMs: 500, what: '种子 job 重建为卡片' },
    );
    // 元数据/失败语义走 list_transfers（UI 同源 TransferDto，job_id=16 位 hex 串）
    const lt = (await A.invoke('list_transfers')).find((j) => String(j.job_id).toLowerCase() === jobSeed.jobHex);
    if (!lt) throw new Error(`list_transfers 无种子 job ${jobSeed.jobHex}`);
    if (lt.state !== 'failed') throw new Error(`state=${lt.state} 应 failed（位图全真→完整性存疑，不可续传）`);
    if (!/完整性存疑/.test(lt.fail_reason || '')) {
      throw new Error(`fail_reason="${lt.fail_reason}" 应含"完整性存疑"`);
    }
    if (lt.done !== lt.total || lt.total !== N_CHUNKS * CHUNK) {
      throw new Error(`done/total 异常: ${lt.done}/${lt.total} 应 ${N_CHUNKS * CHUNK}`);
    }
    if (lt.name !== jobSeed.displayName) throw new Error(`name=${lt.name} 元数据丢失`);
    await A.screenshot(join(runDir, 'v-failed-integrity.png'));
    return { detail: `卡 failed："${lt.fail_reason}"，${lt.done}/${lt.total}B`, artifacts: [F('v-failed-integrity.png')] };
  });

  await step('断言 parts 目录已被启动 GC 删除（=人工"资源管理器核对 parts 消失"）', () => {
    if (existsSync(jobSeed.jobDir)) {
      throw new Error(`位图全真目录未被 GC: ${jobSeed.jobDir}`);
    }
    return `parts 目录 ${jobSeed.jobHex} 已随启动重建清除`;
  });

  await step('断言非可续传：pending_resume_jobs 无 + disk 面非 interrupted + UI 点"续传"被兜底拒绝', async () => {
    const pend = await A.invoke('pending_resume_jobs');
    if ((pend || []).some(([id]) => Number(id) === jobSeed.job)) {
      throw new Error('pending_resume_jobs 仍含该任务——failed 卡不应可续传');
    }
    const dj = (await A.invoke('list_disk_jobs') || []).find((d) => String(d.job_id).toLowerCase() === jobSeed.jobHex);
    if (dj && dj.state === 'interrupted') throw new Error(`disk 面仍 interrupted: ${JSON.stringify(dj)}`);
    // UI：历史区 failed 卡在。产品事实：destination 终态卡恒渲染"续传"按钮
    // （TransferItem 模板不区分 failed/interrupted）——"非可续传"语义由
    // resume_pending 兜底：无 parts manifest → Err"未找到任务"，卡不得复活。
    await A.dismissResumePrompt();
    await A.uiNavigate('/transfers');
    const itemId = `[testid=transfer-item-${jobSeed.jobHex}]`;
    const alreadyVisible = await A.uiWait(itemId, 2000).then(() => true).catch(() => false);
    if (!alreadyVisible) await A.uiClick('[testid=transfers-history-fold-btn]');
    await A.uiWait(itemId, 10_000);
    await A.uiClick(`${itemId} [testid=transfer-resume-pending-btn]`);
    await sleep(2000);
    const after = (await A.invoke('list_transfers')).find((j) => String(j.job_id).toLowerCase() === jobSeed.jobHex);
    if (!after || after.state !== 'failed') {
      throw new Error(`点"续传"后卡状态=${after?.state}——failed 卡被复活，非可续传语义破坏`);
    }
    await A.screenshot(join(runDir, 'v-still-failed-after-resume-click.png'));
    return { detail: `命令面/磁盘面不可续传；UI 点"续传"被 resume_pending 兜底拒绝，卡仍 failed`, artifacts: [F('v-still-failed-after-resume-click.png')] };
  });

  await step('收尾：清 transfers.json 种子条目 + 重启还原 + 证据', async () => {
    await cleanupSeed();
    await restartPc('huss_pc-restore');
    await A.beginTest(SCENARIO);
    const leftover = (await A.invoke('list_transfers')).find((j) => String(j.job_id).toLowerCase() === jobSeed.jobHex);
    if (leftover) throw new Error('清理后种子卡仍复活——transfers.json 条目未清干净');
    const arts = [];
    try {
      await A.screenshot(join(runDir, 'huss_pc-final.png'));
      arts.push(F('huss_pc-final.png'));
    } catch (e) { note(`收尾截图失败（证据缺口，非断言）: ${e.message}`); }
    const ev = await collectEvidence([A], runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    try { await A.endTest('pass'); } catch (e) { note(`test/end 失败（编排器报告为准）: ${e.message}`); }
    return { detail: '种子已清，夹具还原', artifacts: arts };
  });
}

try {
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try { await collectEvidence([A], runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`); } catch { /* 尽力 */ }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  try { await cleanupSeed(); await restartPc('huss_pc-restore'); note('失败兜底：种子清理+重启完成'); }
  catch (e3) { note(`失败兜底清理失败: ${e3.message}`); }
}

const outcome = failureMsg ? 'fail' : (steps.length ? 'pass' : 'skip');
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '★ V1 计划卡 T2：Task12 核对单 #7（手动删 parts 目录后重启 → failed"完整性存疑"而非可续传）',
      '产品行为锚点：位图全真未 finalize = 完整性存疑 → 启动建 failed 卡 + gc_stale_parts 删目录（transfer/mod.rs M-C2）',
      '手工清单"资源管理器核对 parts 目录消失"由本场景 existsSync 文件断言等价覆盖',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
