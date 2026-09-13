// 场景：kill-restart-recovery（V1 计划卡 T2——桌面核对单第 2 条 + 安卓手工清单第 13 项 PC 面等价）
//
// ★★ PC 单机可跑：不依赖 huss_laptop/huss_phone；huss_pc bridge 未就绪时
//    条件 SKIP 退出（exit 0 + 报告 outcome=skip）。已注册 run-all。★★
//
// 验收路径（2026-08-30 传输域重构 Task 12 核对单 #2）：
//   "强杀进程(taskkill)后重启:任务带完整元数据(名字/对端/角色/时间)回归 interrupted,续传可用"
//
// 单机等价做法（与 api-acceptance D2 暂停/继续互补，D2 不覆盖强杀路径——追溯表已注明）：
//   0) huss_pc 不可达 → SKIP（条件跳过设计）。
//   1) 种子"传输中"现场：downloads/.localtrans-parts/<job>/manifest.json
//      （缺块位图 3/8 + meta 全量元数据）——即强杀时刻磁盘上应留下的断点现场。
//   2) 真实 taskkill /F 强杀 localtrans.exe（无优雅退出，等同传输中被杀）。
//   3) 重启 → 断言启动重建卡片：
//      state=interrupted、name=display_name、peer=对端指纹、direction/local_role
//      时间戳保留、done=3×4MiB——"带完整元数据回归 interrupted"。
//   4) 断言续传可用：pending_resume_jobs 含该 job、parts 目录/manifest 完好、
//      list_disk_jobs 面可见；UI 历史区卡片带"续传"按钮（视觉截图）。
//      （不点续传：对端指纹为本机假对端，点击必然失败，非本场景断言面。）
//   5) 收尾：清种子 parts 目录 + transfers.json 对应条目 → 重启还原夹具。
//
// 用法：node scenarios/kill-restart-recovery.mjs（前置：node lib/deploy.mjs --skip-huss_laptop）
import { mkdirSync, existsSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot, repoRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { startPcA, stopPcA, waitReady } from '../lib/deploy.mjs';

const SCENARIO = 'kill-restart-recovery';
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
let jobSeed = null; // { jobHex, jobDir, displayName, createdAtMs, fpSelf, total, done }

const F = (n) => `reports/${runId}/${n}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const CHUNK = 4 * 1024 * 1024; // localtrans-core protocol::CHUNK_SIZE（manifest done 估算同源）
const N_CHUNKS = 8, N_RECV = 3;

/** 可达探测（短超时）：本机 bridge 未起时快速失败 → 调用方走 SKIP 出口 */
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

/** 种子"传输中被强杀"的断点现场：缺块 manifest + 全量 meta（重建唯一真相） */
function seedInterruptedParts() {
  const displayName = `v1-resume-${rand}.bin`;
  const job = 0x5100000000 + (Date.now() % 0xfffff0); // 引擎 ID 段(0x4000/0x8000)之外，安全整数
  const jobHex = job.toString(16).padStart(16, '0'); // 与产品 job_dir/snapshot 的 {:016x} 同形
  const dir = join(downloadDir, '.localtrans-parts', jobHex);
  mkdirSync(dir, { recursive: true });
  const manifest = {
    file_name: displayName,
    total_size: N_CHUNKS * CHUNK,
    chunk_hashes: Array.from({ length: N_CHUNKS }, (_, i) => ('0' + i).repeat(32)),
    received: [...Array(N_RECV).fill(true), ...Array(N_CHUNKS - N_RECV).fill(false)],
    peer: jobSeed.fpSelf,
    meta: {
      direction: 'pull',
      local_role: 'destination',
      display_name: displayName,
      peer_hex: jobSeed.fpSelf, // 单机场景：以本机指纹充当"对端"元数据
      created_at_ms: Date.now(),
      finished_at_ms: null,
      fail_reason: null,
      source_path: null,
      batch_label: null,
    },
  };
  writeFileSync(join(dir, 'manifest.json'), JSON.stringify(manifest, null, 2), 'utf8');
  jobSeed = { ...jobSeed, job: Number(job), jobHex, jobDir: dir, displayName, createdAtMs: manifest.meta.created_at_ms };
  note(`种子断点现场 ${jobHex}（${N_RECV}/${N_CHUNKS} 块）→ ${dir}`);
  return jobSeed;
}

/** 收尾：移除种子 parts 目录 + transfers.json 对应条目（test-side 数据清理） */
async function cleanupSeed() {
  if (jobSeed?.jobDir && existsSync(jobSeed.jobDir)) {
    rmSync(jobSeed.jobDir, { recursive: true, force: true });
  }
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
}

/** 重启 huss_pc（强杀/收尾共用） */
async function restartPc(label) {
  stopPcA();
  startPcA(targets);
  await waitReady(targets.huss_pc, { label, timeoutMs: 120_000 });
  A.runId = null; // 进程已换，run 失效
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口：huss_pc 不可达 → SKIP（不等待）----
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
    return `v${v.appVersion}，download_dir=${downloadDir}，fp=${fpSelf.slice(0, 8)}…`;
  });

  await step(`种子"传输中"断点现场：${N_RECV}/${N_CHUNKS} 块 + 全量 meta（运行中的应用不感知）`, () => {
    const s = seedInterruptedParts();
    return `parts=${s.jobDir}`;
  });

  await step('真实强杀：taskkill /F localtrans.exe（无优雅退出）', async () => {
    stopPcA(); // deploy.mjs：/F 强杀 + pid 死亡确认
    if (!(await reachable(A, 1500))) return '进程已强杀，bridge 无响应（符合预期）';
    throw new Error('taskkill 后 bridge 仍可达——强杀未生效');
  });

  await step('重启 → 断言启动重建卡 interrupted 带完整元数据（名字/对端/角色/时间）', async () => {
    startPcA(targets); // 强杀后进程已不在——脱离编排会话拉起
    await waitReady(targets.huss_pc, { label: 'huss_pc', timeoutMs: 120_000 });
    A.runId = null;
    await A.beginTest(SCENARIO);
    // 元数据断言走 list_transfers（UI 同源 TransferDto：local_role/时间戳只有这里有，
    // state/transfers 摘要面不含）；job_id 序列化为 16 位 hex 字符串
    let lt = null;
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline && !lt) {
      lt = (await A.invoke('list_transfers')).find((j) => String(j.job_id).toLowerCase() === jobSeed.jobHex) ?? null;
      if (!lt) await sleep(500);
    }
    if (!lt) throw new Error(`list_transfers 无种子 job ${jobSeed.jobHex}`);
    const errs = [];
    if (lt.state !== 'interrupted') errs.push(`state=${lt.state} 应 interrupted`);
    if (lt.name !== jobSeed.displayName) errs.push(`name=${lt.name} 应 ${jobSeed.displayName}`);
    if (lt.total !== N_CHUNKS * CHUNK) errs.push(`total=${lt.total} 应 ${N_CHUNKS * CHUNK}`);
    if (lt.done !== N_RECV * CHUNK) errs.push(`done=${lt.done} 应 ${N_RECV * CHUNK}`);
    if (lt.direction !== 'pull') errs.push(`direction=${lt.direction} 应 pull`);
    if (lt.local_role !== 'destination') errs.push(`local_role=${lt.local_role} 应 destination`);
    if ((lt.peer || '') !== jobSeed.fpSelf) errs.push(`peer=${lt.peer} 应为本机指纹`);
    if (!lt.started_at_ms || lt.started_at_ms < jobSeed.createdAtMs) errs.push(`started_at_ms=${lt.started_at_ms} 元数据时间丢失`);
    if (lt.parts_id !== jobSeed.jobHex) errs.push(`parts_id=${lt.parts_id} 应 ${jobSeed.jobHex}`);
    if (errs.length) throw new Error(`重建卡元数据缺口: ${errs.join('; ')}`);
    await A.screenshot(join(runDir, 'v-rebuilt-interrupted.png'));
    return { detail: `卡 ${jobSeed.jobHex} interrupted ${lt.done}/${lt.total}B，元数据全量回归`, artifacts: [F('v-rebuilt-interrupted.png')] };
  });

  await step('断言续传可用：pending_resume_jobs 命中 + parts/manifest 完好 + UI"续传"按钮', async () => {
    const pend = await A.invoke('pending_resume_jobs');
    const hitJob = (pend || []).find(([id, name]) => Number(id) === jobSeed.job || name === jobSeed.displayName);
    if (!hitJob) throw new Error(`pending_resume_jobs 无种子任务: ${JSON.stringify(pend)?.slice(0, 300)}`);
    if (!existsSync(join(jobSeed.jobDir, 'manifest.json'))) throw new Error('重启后 manifest.json 丢失（缺块目录不应被 GC）');
    const disk = await A.invoke('list_disk_jobs');
    const dj = (disk || []).find((d) => String(d.job_id).toLowerCase() === jobSeed.jobHex);
    if (!dj || dj.state !== 'interrupted') throw new Error(`list_disk_jobs 面异常: ${JSON.stringify(dj)}`);
    // UI：中断卡落历史折叠区——展开后应带"续传"按钮（视觉证据）
    await A.dismissResumePrompt(); // 启动恢复提示弹窗（若有）不挡后续 UI
    await A.uiNavigate('/transfers');
    const itemId = `[testid=transfer-item-${jobSeed.jobHex}]`;
    // 折叠态可能被上一轮展开后持久化——卡片已可见则不再点折叠钮（防反向收起）
    const alreadyVisible = await A.uiWait(itemId, 2000).then(() => true).catch(() => false);
    if (!alreadyVisible) await A.uiClick('[testid=transfers-history-fold-btn]');
    await A.uiWait(itemId, 10_000);
    await A.uiWait(`${itemId} [testid=transfer-resume-pending-btn]`, 10_000);
    await A.screenshot(join(runDir, 'v-resume-btn-visible.png'));
    return { detail: `resume 面 job=${hitJob[0]}；磁盘/命令/UI 三面续传可用`, artifacts: [F('v-resume-btn-visible.png')] };
  });

  await step('收尾：清种子数据 + 重启还原 + 证据', async () => {
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
  // 夹具兜底：无论失败点在哪，种子数据必须清掉
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
      '★ V1 计划卡 T2：Task12 核对单 #2（强杀重启 interrupted 带元数据+续传可用）+ 安卓手工清单 #13 PC 面等价',
      '单机等价法：种子缺块 manifest（meta 全量）≈ 强杀时刻磁盘断点现场；taskkill /F 与重启均为真实动作',
      '不点"续传"按钮：种子对端=本机指纹（假对端），续传会话必然失败——续传可用性以 pending_resume_jobs/parts/UI 按钮三面断言',
      'api-acceptance D2 覆盖暂停/继续（不杀进程）；本场景覆盖强杀路径，二者互补不重复',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
