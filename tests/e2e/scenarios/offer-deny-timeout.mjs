// 场景：offer-deny-timeout（V1 计划卡 T1+T2——安卓手工清单 #8 拒绝路径 / #9 超时路径
//        + Task12 核对单 #9 推拉完成 toast）
//
// ★★ 双 PC 场景：待 huss_laptop（192.168.0.222）恢复执行——任一端 bridge 未就绪时
//    自动 SKIP 退出（exit 0 + 报告 outcome=skip）；已注册 run-all。★★
//
// 验收路径：
//   #9 完成 toast：A 推 → B 接收 → A toast"推送完成: <名>" + B toast"下载完成: <名>"
//      （仅活动→终态边触发，main.rs done_toast_text）。
//   #8 拒绝路径：A 推 → B 点"拒绝" → A 卡 failed，fail_reason="推送请求被对方拒绝"
//      （engine.rs EngineError::OfferRejected）。
//   #9 超时路径：A 推 → B 不点（默认 offer_timeout_secs=60 倒计时）→ B 自动拒收并
//      toast"已超时自动拒绝"（App.vue），A 卡同样 failed"推送请求被对方拒绝"。
//
// 夹具约定：不重置身份/信任——会话已 trusted 直接用；互信缺失时走 L3 keepIdentity+
// seedTrust 夹具恢复（与 connect-memory 同款）后再连。收尾 L1 清终态卡（视图级）。
//
// 用法：node scenarios/offer-deny-timeout.mjs（前置：node lib/deploy.mjs 双端部署）
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';

const SCENARIO = 'offer-deny-timeout';
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
let pcFp = null, laptopFp = null;
let fixtureMutated = false; // 失败兜底是否需要恢复夹具

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 唯一内容小文件（头部 runId 戳，避开秒传去重） */
function makeSmallFile(tag) {
  const p = join(e2eRoot(), 'fixtures', 'runs', runId, `odt-${tag}-${rand}.bin`);
  mkdirSync(join(p, '..'), { recursive: true });
  const buf = Buffer.alloc(256 * 1024);
  buf.write(`ODT ${runId} ${tag}`, 0, 'utf8');
  buf.fill(0x6d, 64);
  writeFileSync(p, buf);
  return p;
}

/** 轮询抓 toast 文案（toast 停留数秒，400ms 节拍足够） */
async function pollToast(t, re, windowMs = 12_000) {
  const deadline = Date.now() + windowMs;
  let last = '';
  while (Date.now() < deadline) {
    try {
      last = await t.uiText('.toast');
      if (re.test(last || '')) return last;
    } catch { /* toast 未渲染 */ }
    await sleep(400);
  }
  throw new Error(`${t.name} toast 未命中 /${re.source}/，末次="${String(last).slice(0, 60)}"`);
}

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

/** 夹具恢复：L3 keepIdentity+seedTrust 互播 + 设备名还原 + connect 验证（connect-memory 同款） */
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

/** 会话就位：已 trusted 直用；互信在则 connect；互信缺失才动夹具（L3 播种） */
async function ensureSession() {
  const fpA = await A.invoke('get_device_fingerprint').then((d) => d.fingerprint_hex);
  const fpB = await B.invoke('get_device_fingerprint').then((d) => d.fingerprint_hex);
  pcFp = fpA; laptopFp = fpB;
  const trustedA = (await A.invoke('list_trusted')).some((p) => p.fingerprint === laptopFp);
  const trustedB = (await B.invoke('list_trusted')).some((p) => p.fingerprint === pcFp);
  if (!trustedA || !trustedB) {
    note('互信缺失 → L3 keepIdentity+seedTrust 夹具恢复');
    fixtureMutated = true;
    await restorePcFixture();
    return '夹具播种后 connect trusted';
  }
  const hasSession = (await A.state()).sessions.some((x) => x.peer === laptopFp && x.trusted);
  if (!hasSession) {
    try { await A.invoke('connect', { fingerprint: laptopFp }); }
    catch { await sleep(3000); await A.invoke('connect', { fingerprint: laptopFp }); }
  }
  await A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
    { timeoutMs: 30_000, what: 'huss_pc 会话 trusted' });
  return '既有互信任用，会话 trusted';
}

/** A 推一个文件 → 返回占位卡 id（接收动作由调用方决定） */
async function pushFromA(tag) {
  const f = makeSmallFile(tag);
  const cardId = await A.invoke('push_files', { fingerprint: laptopFp, local_paths: [f] });
  note(`push ${tag} 占位卡 ${cardId}`);
  return { cardId, file: f };
}

/** A 卡等真终态并断言失败语义 */
async function assertSenderFailed(cardId, what) {
  const hit = await A.pollUntil(
    (s) => {
      const t = s.transfers.find((x) => x.id === cardId);
      return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
    },
    { timeoutMs: 90_000, intervalMs: 1000, what },
  );
  const c = hit.value;
  if (c.state !== 'failed') throw new Error(`${what}: state=${c.state} 应 failed`);
  if (!/拒绝/.test(c.fail_reason || '')) throw new Error(`${what}: fail_reason="${c.fail_reason}" 应含"拒绝"`);
  return c.fail_reason;
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口：任一 PC 不可达 → SKIP ----
  const aUp = await reachable(A);
  const bUp = aUp ? await reachable(B) : false;
  if (!aUp || !bUp) {
    const msg = `SKIP：${!aUp ? 'huss_pc' : 'huss_laptop'} 不可达（bridge 未就绪）——待设备恢复后补跑双机证据`;
    console.log(`\n[${SCENARIO}] ${msg}`);
    note(msg);
    writeReport({
      runId, scenario: SCENARIO, steps: [{ name: '可达探测', status: 'SKIP', detail: msg, durationMs: 0 }],
      outcome: 'skip',
      env: {
        runDir, startedAt: startedAt.toISOString(), finishedAt: new Date().toISOString(),
        durationMs: Date.now() - startedAt, versions,
        notes: ['条件跳过设计：任一 PC bridge 未就绪即 SKIP 退出（exit 0），run-all 注册表照常通过'],
      },
    });
    return;
  }

  await step('环境握手：双 PC bridge 就绪（记录指纹/版本）', async () => {
    for (const t of both) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      versions[t.name] = v;
    }
    const a = await A.beginTest(SCENARIO);
    const b = await B.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…`;
  });

  await step('会话就位：互信任用（缺失才 L3 播种）→ connect trusted', async () => {
    const d = await ensureSession();
    await L1Clear();
    return d;
  });

  await step('【完成 toast】A 推 → B 接收 → 双端"推送完成/下载完成"toast', async () => {
    const { cardId } = await pushFromA('accept');
    await B.dismissResumePrompt();
    await B.uiWait('.offer-modal', 20_000);
    await B.uiClick('.offer-modal .btn-success');
    await A.pollUntil(
      (s) => s.transfers.some((t) => t.id === cardId && t.state === 'done'),
      { timeoutMs: 120_000, intervalMs: 500, what: 'A 卡 done' },
    );
    await B.pollUntil(
      (s) => s.transfers.some((t) => t.direction === 'pull' && t.state === 'done'),
      { timeoutMs: 120_000, intervalMs: 500, what: 'B 接收卡 done' },
    );
    // 完成事件已落——toast 可能刚弹出，双向并行抓取
    const [ta, tb] = await Promise.all([
      pollToast(A, /推送完成/).catch((e) => { note(e.message); throw e; }),
      pollToast(B, /下载完成/).catch((e) => { note(e.message); throw e; }),
    ]);
    await A.screenshot(join(runDir, 'v-toast-push-done.png'));
    return { detail: `A:"${ta.trim().slice(0, 30)}" / B:"${tb.trim().slice(0, 30)}"`, artifacts: [F('v-toast-push-done.png')] };
  });

  await step('【拒绝路径】A 推 → B 点"拒绝" → A 卡 failed"推送请求被对方拒绝"', async () => {
    const { cardId } = await pushFromA('deny');
    await B.uiWait('.offer-modal', 20_000);
    await B.screenshotSoft(join(runDir, 'v-offer-before-deny.png'));
    await B.uiClick('.offer-modal .btn-secondary'); // 拒绝（App.vue handleRejectOffer）
    const reason = await assertSenderFailed(cardId, '拒绝路径 A 卡');
    await A.screenshot(join(runDir, 'v-sender-rejected.png'));
    return { detail: `A 卡 fail_reason="${reason}"`, artifacts: [F('v-sender-rejected.png'), F('v-offer-before-deny.png')] };
  });

  await step('【超时路径】A 推 → B 不点（60s 倒计时自动拒收）→ B toast"已超时自动拒绝"+ A 卡 failed', async () => {
    const { cardId } = await pushFromA('timeout');
    await B.uiWait('.offer-modal', 20_000);
    const countdown = await B.uiText('.offer-countdown').catch(() => '');
    note(`B 倒计时显示: "${String(countdown).trim().slice(0, 40)}"`);
    await B.screenshotSoft(join(runDir, 'v-offer-countdown.png'));
    // 不点：等 offer 超时自动拒收（模态消失 + toast）
    let modalGone = false;
    for (let i = 0; i < 40 && !modalGone; i++) { // ≤80s
      await sleep(2000);
      const still = await B.uiText('.offer-modal').then(() => true).catch(() => false);
      modalGone = !still;
    }
    if (!modalGone) throw new Error('80s 内 offer 模态未消失——超时自动拒收未生效');
    const tb = await pollToast(B, /已超时自动拒绝/, 8_000);
    const reason = await assertSenderFailed(cardId, '超时路径 A 卡');
    await B.screenshotSoft(join(runDir, 'v-after-timeout.png'));
    return { detail: `B:"${tb.trim().slice(0, 30)}"；A 卡 fail_reason="${reason}"`, artifacts: [F('v-after-timeout.png'), F('v-offer-countdown.png')] };
  });

  await step('收尾：L1 清终态卡 + 证据 + test/end', async () => {
    await L1Clear();
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

/** L1 软清（终态卡视图级清理，数据保留） */
async function L1Clear() {
  for (const t of both) {
    try { await t.invoke('clear_completed_transfers'); } catch (e) { note(`${t.name} L1 清理失败: ${e.message}`); }
  }
}

try {
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try { await collectEvidence(both, runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`); } catch { /* 尽力 */ }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  try { await B.endTest('fail'); } catch { /* 尽力 */ }
  if (fixtureMutated && pcFp && laptopFp) {
    console.log(`[${SCENARIO}] 失败兜底：恢复 PC 信任夹具…`);
    try { await restorePcFixture(); note('失败兜底夹具恢复完成'); }
    catch (e3) { note(`失败兜底夹具恢复失败: ${e3.message}`); }
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
      '★ V1 计划卡 T1/T2：安卓手工清单 #8 拒绝路径、#9 超时路径 + Task12 核对单 #9 推拉完成 toast',
      '超时路径用默认 offer_timeout_secs=60（不改设置，零夹具风险）：B 倒计时自然到期自动拒收',
      'toast 断言依赖 toast 停留时长（~3s 量级），400ms 轮询节拍；完成断言先等终态再抓 toast，避免竞态窗口误判',
      '发送端卡在拒绝/超时后同为 failed"推送请求被对方拒绝"（OfferRejected）——语义区分点在接收端 toast',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
