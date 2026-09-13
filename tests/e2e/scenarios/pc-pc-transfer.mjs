// 场景：pc-pc-transfer（M6 首个真传输场景——spec §7.4.2 示例的落地）
//
// 流程：双端 begin → 双向发现 → 配对（A invoke connect 发起；B UI 点 btn-grant
// 真实同意；A UI 输配对码提交——配对确认刻意不走 invoke，保真实用户流程）→
// A invoke push_files 推 4 个 fixtures → B UI 点"接收"（push=Ask 真实弹窗）→
// 双端断言终态/字节数/文件数 → 截图+证据+报告。
//
// 任何一步失败：collectEvidence 后报告标 FAIL，退出码 1。
// 用法：node scenarios/pc-pc-transfer.mjs   （前置：node lib/deploy.mjs 已部署双端）
import { mkdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { ensureFixtures, genRunVariants } from '../fixtures/gen.mjs';

const SCENARIO = 'pc-pc-transfer';
// 秒级时间 + 随机尾缀：runId 唯一性必须强于分钟——fixture 变体按 runId 加戳，
// 同分钟重跑若戳相同，接收端收件箱秒传（按内容 hash）会合法地跳过传输，
// "全量真传输"断言失真（实测踩中）
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19); // 秒级
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const huss_pc = new Target('huss_pc', targets.huss_pc, journal);
const huss_laptop = new Target('huss_laptop', targets.huss_laptop, journal);

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let fixtureFiles = [];
let expectedBytes = 0;
let fpA = null; // huss_pc 指纹（huss_laptop 视角）
let fpB = null; // huss_laptop 指纹（huss_pc 视角）
let pairedThisRun = false;

const F = (n) => `reports/${runId}/${n}`; // 报告内相对链接（report.md 位于 runDir）
const note = (m) => journal.append({ kind: 'note', message: m });

/** 步骤包装：PASS/FAIL 计时入矩阵；失败上抛由顶层 catch 统一收割证据。
 * fn 可返回 string（detail）或 {detail, artifacts: [相对链接]} */
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

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  await step('环境握手：双端 health/version', async () => {
    for (const t of [huss_pc, huss_laptop]) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      if (v.apiVersion !== 1) throw new Error(`${t.name} apiVersion=${v.apiVersion}`);
      versions[t.name] = v;
    }
    return `huss_pc v${versions.huss_pc.appVersion}/${versions.huss_pc.buildProfile}，huss_laptop v${versions.huss_laptop.appVersion}/${versions.huss_laptop.buildProfile}`;
  });

  await step('L2 重置 + test/begin + 双向发现', async () => {
    // L2(进程重启)而非 L1:链式场景下前站可能残留活动引擎/未落终态的卡,
    // L1 的 view 级清理对 active 卡无效——重启是唯一干净的起点。
    // begin 必须在重启后(重启清 test run 上下文,先前置 begin 会 step 400)
    await reset(2, [huss_pc, huss_laptop]);
    // L2 后 laptop 可能弹断点恢复提示(挡 offer-modal),先关掉
    await huss_laptop.dismissResumePrompt();
    await huss_pc.dismissResumePrompt();
    const runA = await huss_pc.beginTest(SCENARIO);
    const runB = await huss_laptop.beginTest(SCENARIO);
    // L2 恢复 transfers.json 会把上轮 done 卡带回视图——genRunVariants 文件名
    // 不带 runId(同名不同内容),旧卡会污染本 run 的按名断言,L1 软清一次
    await reset(1, [huss_pc, huss_laptop]);
    await Promise.all([
      huss_pc.waitState({ path: 'devices.length', op: 'gte', value: 1, timeoutMs: 20_000 }),
      huss_laptop.waitState({ path: 'devices.length', op: 'gte', value: 1, timeoutMs: 20_000 }),
    ]);
    const sA = await huss_pc.state();
    const sB = await huss_laptop.state();
    const b = sA.devices.find((d) => d.name === 'huss_laptop');
    const a = sB.devices.find((d) => d.name === 'huss_pc');
    if (!b) throw new Error(`huss_pc 未发现 HUSS，实际: ${sA.devices.map((d) => d.name).join(',')}`);
    if (!a) throw new Error(`huss_laptop 未发现 huss_pc，实际: ${sB.devices.map((d) => d.name).join(',')}`);
    if (!b.addr || !b.addr.includes(':')) throw new Error(`huss_pc 视角 HUSS.addr 异常: ${JSON.stringify(b.addr)}`);
    fpB = b.id;
    fpA = a.id;
    return `huss_pc 见 huss_laptop@${b.addr}，huss_laptop 见 huss_pc（addr 钉死断言通过）`;
  });

  await step('配对：connect 发起 + B UI 同意 + A UI 输码', async () => {
    await huss_pc.invoke('connect', { fingerprint: fpB });
    // 历史信任命中：connect 直接建会话，B 不弹配对框 → 跳过 UI 配对
    try {
      await huss_pc.waitState({ path: 'sessions.length', op: 'gte', value: 1, timeoutMs: 8_000 });
      const s = await huss_pc.state();
      if (s.sessions.some((x) => x.peer === fpB && x.trusted)) {
        return '历史信任命中：会话已建立（跳过配对 UI）';
      }
    } catch { /* 8s 无会话 → 走配对路径 */ }

    // B 侧同意门（consent_timeout 窗口内点击，默认 60s）
    await huss_laptop.step('pairing-gate');
    await huss_laptop.uiWait('[testid=pairing-dialog]', 15_000);
    await huss_laptop.uiClick('[testid=btn-grant]');

    // 读 B 屏上亮出的配对码（CodeBadge 渲染 "12 34 56"）；失败兜底白名单只读命令
    let code;
    try {
      const txt = await huss_laptop.uiText('.code-display');
      code = (txt || '').replace(/\D/g, '');
    } catch { /* 兜底 */ }
    if (!/^\d{6}$/.test(code || '')) {
      const pending = await huss_laptop.invoke('get_pairing_pending');
      const mine = (pending || []).find((p) => p.fingerprint === fpA);
      code = (mine?.own_code || '').replace(/\D/g, '');
    }
    if (!/^\d{6}$/.test(code || '')) throw new Error(`配对码读取失败: ${JSON.stringify(code)}`);

    // A 侧输码提交（真实输码流程，配对确认不走 invoke）
    await huss_pc.uiWait('[testid=code-input]', 20_000);
    await huss_pc.uiInput('[testid=code-input]', code);
    await huss_pc.uiClick('[testid=btn-submit]');

    // 双端会话 + 信任达成（配对完成写信任表后 SessionUp）
    await Promise.all([
      huss_pc.pollUntil((s) => s.sessions.some((x) => x.peer === fpB && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc sessions 含 HUSS 且 trusted' }),
      huss_laptop.pollUntil((s) => s.sessions.some((x) => x.peer === fpA && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop sessions 含 huss_pc 且 trusted' }),
    ]);
    pairedThisRun = true;
    return `配对完成（码 ${code.slice(0, 2)}****），双端 sessions trusted=true`;
  });

  await step('传输：push_files 4 fixtures → B UI 接收', async () => {
    await huss_pc.step('push-files');
    await huss_laptop.step('offer-accept');
    ensureFixtures(); // 基准 fixture 就位（runs 变体由其拷贝而来）
    // 运行期唯一变体：内容带 runId 戳，规避接收端内容去重（秒传）——
    // 重复运行也保持"4 文件全量真实传输"的确定性（去重路径留独立场景）
    fixtureFiles = genRunVariants(runId);
    const card = await huss_pc.invoke('push_files', { fingerprint: fpB, local_paths: fixtureFiles });
    note(`push_files 占位卡片 ${card}，文件 ${fixtureFiles.length} 个`);

    // B 侧 push=Ask：接收确认弹窗 → UI 点"接收"（offer 模态未铺 testid——
    // ui/src 归另一泳道，跨泳道约束下用 CSS 直选，记入报告备注）
        await huss_laptop.dismissResumePrompt();
await huss_laptop.uiWait('.offer-modal', 20_000);
    await huss_laptop.uiClick('.offer-modal .btn-success');
    return `占位卡 ${card}，B 已点接收`;
  });

  await step('断言：双端终态/字节数/文件数', async () => {
    // A 侧父卡终态（批次 4 文件 → 单父卡；total 协商后 > 0）
    const aSnap = await huss_pc.pollUntil(
      (s) => s.transfers.length > 0 && s.transfers.every((t) => t.state === 'done') && s.transfers[0].bytesTotal > 0,
      { timeoutMs: 120_000, intervalMs: 500, what: 'huss_pc 推送卡全部 done' },
    );
    const aCard = aSnap.snapshot.transfers.find((t) => t.direction === 'push');
    if (!aCard) throw new Error('huss_pc 无 push 卡片');
    if (aCard.bytesDone !== aCard.bytesTotal) {
      throw new Error(`huss_pc bytesDone(${aCard.bytesDone}) != bytesTotal(${aCard.bytesTotal})`);
    }
    if (aCard.failReason) throw new Error(`huss_pc 卡片失败: ${aCard.failReason}`);

    // B 侧接收卡全部终态（空文件 total=0，done=total 亦成立）。
    // 注：接收事件泵建卡时 peer 置空（main.rs recv_rx 泵："乙侧不知道对端指纹
    // ——编排任务未传"，已知产品限制）——按 direction=pull 识别（L1 重置后
    // 视图内 pull 卡即本次接收），文件名覆盖断言钉死归属。
    const bSnap = await huss_laptop.pollUntil(
      (s) => {
        const mine = s.transfers.filter((t) => t.direction === 'pull');
        return mine.length > 0 && mine.every((t) => t.state === 'done');
      },
      { timeoutMs: 120_000, intervalMs: 500, what: 'huss_laptop 接收卡全部 done' },
    );
    // 只认本 run 的接收卡(名字含 fixture 名)——视图可能残留其他 run 的
    // pull 卡(浏览批次/上轮未清),硬等数会误伤
    const pairs0 = fixtureFiles.map((p) => ({ path: p, name: p.split(/[\\/]/).pop() }));
    const bCards = bSnap.snapshot.transfers.filter((t) => t.direction === 'pull'
      && pairs0.some(({ name }) => (t.name || '').includes(name) || name.includes(t.name || '\u0000')));
    for (const c of bCards) {
      if (c.bytesDone !== c.bytesTotal) throw new Error(`huss_laptop 卡 ${c.name}: done(${c.bytesDone}) != total(${c.bytesTotal})`);
      if (c.failReason) throw new Error(`huss_laptop 卡 ${c.name} 失败: ${c.failReason}`);
    }

    // 文件名覆盖：B 侧接收卡名字须覆盖 4 个 fixture 名（中文名/空文件都在内）。
    // 用 Rust 快照（权威源）——Pinia 聚合快照经 4Hz 事件更新，轮询命中瞬间
    // 可能滞后（实测复跑踩中：state/app transfers 为空而 Rust 侧已 done）。
    const bNames = new Set(bCards.map((c) => c.name));
    const pairs = pairs0;
    const missing = pairs.filter(({ name }) => ![...bNames].some((bn) => bn.includes(name) || name.includes(bn)));
    if (missing.length) throw new Error(`huss_laptop 缺文件: ${missing.map((m) => m.name).join(',')}；实收 [${[...bNames].join(' | ')}]`);

    // 字节数：fixture 实际字节合计 == A 父卡 total == B 接收卡 total 合计；文件数一致
    expectedBytes = pairs.reduce((s, p) => s + statSync(p.path).size, 0);
    if (bCards.length !== pairs.length) {
      throw new Error(`huss_laptop 接收卡 ${bCards.length} 张 != 文件数 ${pairs.length}`);
    }
    const bTotal = bCards.reduce((s, c) => s + c.bytesTotal, 0);
    if (bTotal !== expectedBytes) throw new Error(`huss_laptop 字节合计 ${bTotal} != fixtures ${expectedBytes}`);
    if (aCard.bytesTotal !== expectedBytes) throw new Error(`huss_pc 父卡 total ${aCard.bytesTotal} != fixtures ${expectedBytes}`);
    return `${pairs.length} 文件 ${expectedBytes}B 双端 done；A 卡=${aCard.id}，B 卡 x${bCards.length}`;
  });

  await step('收尾：截图 + 证据 + test/end', async () => {
    const arts = [];
    for (const t of [huss_pc, huss_laptop]) {
      await t.screenshotSoft(join(runDir, `${t.name}-final.png`));
      arts.push(F(`${t.name}-final.png`));
    }
    const ev = await collectEvidence([huss_pc, huss_laptop], runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    await huss_pc.endTest('pass');
    await huss_laptop.endTest('pass');
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
    const ev = await collectEvidence([huss_pc, huss_laptop], runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
    void ev;
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await huss_pc.endTest('fail'); } catch { /* 尽力 */ }
  try { await huss_laptop.endTest('fail'); } catch { /* 尽力 */ }
}

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      pairedThisRun ? '本次完成全新配对（B UI 同意 + A UI 输码）' : '命中历史信任，跳过配对 UI',
      '配对确认/接收确认走 UI 点击（真实流程）；connect/push_files 为白名单语义注入',
      'B 侧接收弹窗按钮用 CSS 直选（.offer-modal .btn-success）——该模态未铺 testid，ui/ 属并行泳道不越界',
      '已知限制：huss_laptop 接收卡 peer 为空（接收事件泵未传对端指纹，main.rs 注释明示）——场景按 direction+文件名覆盖识别，留 M8 评估产品侧补齐',
      'fixture 用运行期唯一变体（内容带 runId 戳）规避接收端内容去重（秒传）——重复运行保持全量真传输；重复推送曾暴露秒传卡卡死 pending 的产品 bug（本次已修 main.rs InstantHit 泵：先 Started 再 Finished），去重行为本身留独立场景验证',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
if (outcome === 'pass') {
  // 收尾清场：双端终态卡片 view 级清理，机位干净（进程保持运行——与任务前一致）
  try { await reset(1, [huss_pc, huss_laptop]); } catch (e) { note(`收尾 L1 清理失败: ${e.message}`); }
}
process.exit(outcome === 'pass' ? 0 : 1);
