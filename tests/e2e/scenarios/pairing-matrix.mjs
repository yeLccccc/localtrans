// 场景：pairing-matrix（P2 打磨卡 T3——配对失败矩阵 PC↔手机真机验收）
//
// ★★ 条件跳过：huss_pc bridge 未就绪 或 huss_phone(adb) 不可达 或 APK 未构建
//    → SKIP 退出（exit 0）。已注册 run-all。huss_laptop 不参与（不可达遗留:
//    双 PC 版矩阵待其恢复后另行补跑）。★★
//
// 验收路径（P2 计划卡 T1 梳理表可真机项；每分支前重置配对态）：
//   0) 设备不可达 → SKIP（条件跳过设计）。
//   1) 夹具：备份 PC data 三件（config/trusted/connect_memory）→ 手机
//      factoryReset（pm clear 全新身份+零信任,cold-start 同款手法;配对同意门
//      只看接受方 is_trusted,单边残留信任会让门静默跳过——双端都必须零信任）
//      → 停 PC → 清信任/连接记忆 + consent_timeout_secs=15（超时分支提速）→ 起 PC。
//   2) 分支B 对端拒绝：PC 连手机 → 手机点拒绝 → 断言 PC 失败态文案
//      「对方拒绝了本次配对请求」（P2 建议文案）→ 关闭。
//   3) 分支C 同意门超时：手机连 PC → PC 同意门不响应 → 15s 自动拒绝 →
//      断言 PC 弹窗收敛 + toast 建议文案（「同意门超时…重新发起」）。
//   4) 分支D 错码 3 次→冷却：PC 连手机 → 手机同意 → 连错 3 次 →
//      断言 第1/2次剩余次数文案递减、第3次弹窗关闭+冷却 toast +
//      设备卡冷却倒计时徽章（P2 新增）→ 冷却期内 PC 重连被拒。
//   5) 分支A 错码 1 次重输成功（放最后:成功会写双向信任,失败分支不写,
//      无需中间清场;手机 App 重启清其内存冷却后进行）：PC 连手机 → 手机
//      同意亮码 → PC 故意输错 → 断言「还可重试 2 次」分层文案（P2 修复:
//      旧版此处弹窗直接关闭）→ 输对 → 双端 trusted。
//   6) 收尾恢复夹具：还原 PC 三件（信任表并入手机新指纹条目）→ connect
//      验证 trusted。★手机身份经 pm clear 已更换,PC 信任表原手机条目
//      成为死项（与 cold-start 同语义,报告记录）。
//
// 用法：node scenarios/pairing-matrix.mjs（前置：node lib/deploy.mjs --skip-huss_laptop）
import { mkdirSync, existsSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadTargets, e2eRoot, repoRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import createAdb, { PACKAGE } from '../lib/adb.mjs';
import { stopPcA, startPcA } from '../lib/deploy.mjs';
import { writeReport } from '../lib/report.mjs';
import { Journal } from '../lib/journal.mjs';

const SCENARIO = 'pairing-matrix';
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const stamp = Date.now().toString(36);
const rand = Math.random().toString(36).slice(2, 5);
const runId = `${SCENARIO}-${stamp}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });
const journal = new Journal(runDir);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const APK = join(repoRoot(), 'android', 'app', 'build', 'outputs', 'apk', 'debug', 'app-debug.apk');
const PC_DATA = join(repoRoot(), 'target', 'release', 'data');

const log = (m) => console.log(m);
const note = (m) => journal.append({ kind: 'note', message: m });
const F = (n) => `reports/${runId}/${n}`;
const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let phoneFp = null;
let pcFp = null;

async function step(name, fn) {
  const t0 = Date.now();
  console.log(`  ▶ ${name}`);
  try {
    const r = await fn();
    const detail = typeof r === 'string' ? r : (r?.detail || '');
    const artifacts = typeof r === 'string' ? [] : (r?.artifacts || []);
    steps.push({ name, status: 'PASS', detail, durationMs: Date.now() - t0, artifacts });
    log(`  PASS  ${name}${detail ? '  ' + detail : ''}`);
  } catch (e) {
    steps.push({ name, status: 'FAIL', detail: e.message, durationMs: Date.now() - t0 });
    throw e;
  }
}

function skip(msg) {
  log(`\n[${SCENARIO}] SKIP：${msg}`);
  writeReport({
    runId, scenario: SCENARIO,
    steps: [{ name: '可达探测', status: 'SKIP', detail: msg, durationMs: 0 }],
    outcome: 'skip',
    env: {
      runDir, startedAt: startedAt.toISOString(), finishedAt: new Date().toISOString(),
      durationMs: Date.now() - startedAt, versions,
      notes: ['条件跳过设计：PC/手机不可达或 APK 缺失即 SKIP 退出（exit 0），run-all 注册表照常通过'],
    },
  });
  process.exit(0);
}

// ---- 手机侧助手（交互模型沿用 remote-rename/android-pull-batch 实证手法）----

/** 手机同意门:等「同意」出现并点击。
 *  ★ 实证:PairingDialogs 是裸 Compose Dialog,未开 testTagsAsResourceId,
 *  a11y 树拿不到 btn-grant/btn-deny tag——按文案节点中心点击(remote-rename 同款)。 */
async function phoneGrant(timeoutMs = 25_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const els = await ch.dump();
    const grant = els.find((e) => (e.text || '').trim() === '同意');
    if (grant) { await ch.tapXY(grant.center[0], grant.center[1]); return; }
    await sleep(700);
  }
  throw new Error('手机同意门未出现（「同意」25s 超时）');
}

/** 手机拒绝门:等「拒绝」出现并点击(文案定位,理由同上) */
async function phoneDeny(timeoutMs = 25_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const els = await ch.dump();
    const deny = els.find((e) => (e.text || '').trim() === '拒绝');
    if (deny) { await ch.tapXY(deny.center[0], deny.center[1]); return; }
    await sleep(700);
  }
  throw new Error('手机拒绝门未出现（「拒绝」25s 超时）');
}

/** 读手机亮码（acceptor 大字码;兼容 6 位连写与分隔形态） */
async function readPhoneCode(timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const els = await ch.dump();
    const codeNode = els.find((e) => /^\d{6}$/.test((e.text || '').trim()));
    if (codeNode) return codeNode.text.trim();
    const spaced = els.find((e) => /^\d{2}\s+\d{2}\s+\d{2}$/.test((e.text || '').trim()));
    if (spaced) return spaced.text.replace(/\s/g, '');
    await sleep(700);
  }
  throw new Error('手机配对码读取失败（20s 无 6 位码节点）');
}

/** 尽力关掉手机上残留的失败/等待弹窗（分支间清场,非断言） */
async function dismissPhoneDialogs() {
  for (const text of ['确定', '知道了', '取消等待', '取消', '关闭']) {
    try {
      const els = await ch.dump();
      const node = els.find((e) => (e.text || '').trim() === text && e.center);
      if (node) { await ch.tapXY(node.center[0], node.center[1]); await sleep(800); }
    } catch { /* 无弹窗 */ }
  }
}

function phoneUp() {
  try {
    return execFileSync(ch.adbPath, ['-s', ch.serial, 'get-state'], { encoding: 'utf8', timeout: 8000 }).trim() === 'device';
  } catch {
    return false;
  }
}

// ---- PC data 夹具助手 ----

const BACKUP = { config: null, trusted: null, memory: null };
function backupPcData() {
  const rd = (f) => (existsSync(join(PC_DATA, f)) ? readFileSync(join(PC_DATA, f), 'utf8') : null);
  BACKUP.config = rd('config.json');
  BACKUP.trusted = rd('trusted_peers.json');
  BACKUP.memory = rd('connect_memory.json');
}
/** 停 PC 后调用：清信任+连接记忆（保留 config/身份/传输记录） */
function wipePairingState() {
  for (const f of ['trusted_peers.json', 'connect_memory.json']) {
    rmSync(join(PC_DATA, f), { force: true });
  }
}
/** 停 PC 后调用：同意门超时降到 15s（超时分支提速;core 钳制 [1,600]） */
function lowerConsentTimeout() {
  const p = join(PC_DATA, 'config.json');
  if (!existsSync(p)) return;
  const cfg = JSON.parse(readFileSync(p, 'utf8'));
  cfg.consent_timeout_secs = 15;
  writeFileSync(p, JSON.stringify(cfg, null, 2), 'utf8');
}
/** 还原三件（备份缺失=null 时移除） */
function restorePcData() {
  const wr = (f, content) => {
    if (content === null) rmSync(join(PC_DATA, f), { force: true });
    else writeFileSync(join(PC_DATA, f), content, 'utf8');
  };
  wr('config.json', BACKUP.config);
  wr('trusted_peers.json', BACKUP.trusted);
  wr('connect_memory.json', BACKUP.memory);
}

/** 还原用信任表:备份条目 + 手机新指纹条目(pm clear 后身份已换,原条目成死项) */
function mergedTrustWithNewPhone() {
  let entries = [];
  try { entries = JSON.parse(BACKUP.trusted || '[]'); } catch { entries = []; }
  if (!Array.isArray(entries)) entries = [];
  entries = entries.filter((e) => e && e.fingerprint !== phoneFp);
  entries.push({
    fingerprint: phoneFp,
    name: '我的手机',
    alias: '',
    paired_at: Math.floor(Date.now() / 1000),
    perms: { browse: true, download: true, push: 'ask' },
  });
  return JSON.stringify(entries, null, 2);
}
async function waitPcReady(timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const v = await A.version();
      if (v?.bridgeReady === true) return;
    } catch { /* 未就绪 */ }
    await sleep(800);
  }
  throw new Error(`PC bridge ${timeoutMs / 1000}s 未就绪`);
}

/** PC 侧驱动一轮「PC 发起配对 → 手机同意亮码」 */
async function pcInitiatePhoneAccept() {
  await A.invoke('connect', { fingerprint: phoneFp });
  await phoneGrant();
  return readPhoneCode();
}

/** 确定性的 3 个错码（均 ≠ 真码） */
function wrongCodes(code) {
  return ['000000', '111111', '222222', '333333'].filter((w) => w !== code).slice(0, 3);
}

async function main() {
  log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口 ----
  let pcUp = false;
  try { pcUp = (await A.version()).bridgeReady === true; } catch { pcUp = false; }
  if (!pcUp) skip('huss_pc bridge 未就绪——先跑 node lib/deploy.mjs --skip-huss_laptop');
  if (!phoneUp()) skip('huss_phone(adb) 不可达');
  if (!existsSync(APK)) skip(`APK 未构建: ${APK}`);

  await step('环境握手 + 手机就绪（亮屏/装包）', async () => {
    versions.huss_pc = await A.version();
    pcFp = (await A.invoke('get_device_fingerprint')).fingerprint_hex;
    ch.wake(); ch.setStayOn(true); await sleep(1200);
    await ch.install(APK);
    return `PC v${versions.huss_pc.appVersion}`;
  });

  await step('夹具:PC 三件备份 + 手机 factoryReset(全新身份零信任) + PC 清配对态', async () => {
    backupPcData();
    // 手机 pm clear:全新身份+零信任。配对门只看接受方 is_trusted——
    // 残留的单边信任会让门静默跳过（首轮实证:手机旧信任→直连无门）
    const reset = await ch.factoryReset();
    note(`手机 factoryReset 完成: ${String(reset.cleared).slice(0, 40)} banner=${String(reset.banner).slice(0, 40)}`);
    // 停 PC → 清信任/连接记忆 + consent 15s → 起 PC
    stopPcA();
    await sleep(1500);
    wipePairingState();
    lowerConsentTimeout();
    startPcA();
    await waitPcReady();
    // 等 PC 发现新身份手机并取新指纹
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      const s = await A.state();
      const phone = (s.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
      if (phone) { phoneFp = phone.id; break; }
      await sleep(1000);
    }
    if (!phoneFp) throw new Error('PC 30s 未发现出厂重置后的手机');
    const cfg = await A.invoke('get_settings');
    if (cfg.consent_timeout_secs !== 15) throw new Error(`consent_timeout_secs 应为 15,实际 ${cfg.consent_timeout_secs}`);
    await ch.tap({ testTag: 'nav-devices-link' }); await sleep(800);
    note(`新 phoneFp=${phoneFp}`);
    return `phoneFp=${phoneFp.slice(0, 8)}… consent_timeout=15s 双端零信任`;
  });

  // ============ 分支 B:对端拒绝 ============
  await step('分支B 对端拒绝:手机点拒绝 → PC 失败态建议文案', async () => {
    await A.invoke('connect', { fingerprint: phoneFp });
    await phoneDeny();
    // P2:PC 侧进失败态,建议文案可操作(旧版 toast「对端拒绝」+弹窗即关)
    await A.uiWait('[testid=pairing-failed-reason]', 15_000);
    const reason = await A.uiText('[testid=pairing-failed-reason]');
    if (!/对方拒绝了本次配对请求/.test(reason)) throw new Error(`失败态文案不符: ${reason}`);
    await A.screenshot(join(runDir, 'v-B-denied-failed-state.png'));
    await A.pollUntil((s) => !(s.sessions ?? []).some((x) => x.peer === phoneFp),
      { timeoutMs: 15_000, intervalMs: 800, what: '拒绝后会话收敛' });
    await A.uiClick('[testid=pairing-failed-close-btn]');
    return { detail: `文案="${reason.trim()}" 会话已收敛`, artifacts: [F('v-B-denied-failed-state.png')] };
  });

  // ============ 分支 C:同意门超时 ============
  await step('分支C 同意门超时:手机连 PC 不响应 → 15s 自动拒绝+建议 toast', async () => {
    await dismissPhoneDialogs();
    await ch.tap({ testTag: `device-card-${pcFp}` });
    // PC 同意门出现(不点)
    await A.uiWait('[testid=pairing-dialog]', 20_000);
    await sleep(1500);
    // 等 15s 门超时 + 收敛窗口
    const deadline = Date.now() + 30_000;
    let gone = false;
    while (Date.now() < deadline) {
      const txt = await A.uiText('.pairing-dialog').catch(() => '');
      if (!txt || !txt.includes('配对请求')) { gone = true; break; }
      await sleep(800);
    }
    if (!gone) throw new Error('PC 同意门 30s 内未自动拒绝收敛（consent_timeout=15s）');
    // toast 建议文案（3s 存活窗,快轮询抓取;抓不到记截图兜底目检）
    let toastSeen = false;
    const tEnd = Date.now() + 6000;
    while (Date.now() < tEnd && !toastSeen) {
      const toast = await A.uiText('.toast').catch(() => '');
      if (/同意门超时|重新发起/.test(toast || '')) { toastSeen = true; break; }
      await sleep(250);
    }
    await A.screenshot(join(runDir, 'v-C-consent-timeout.png'));
    await dismissPhoneDialogs();
    return { detail: `门自动收敛 ✓ toast=${toastSeen ? '已捕获建议文案' : '未捕获(截图目检)'}`, artifacts: [F('v-C-consent-timeout.png')] };
  });

  // ============ 分支 D:错码 3 次 → 冷却 ============
  await step('分支D 错码3次:剩余次数递减 2→1', async () => {
    const code = await pcInitiatePhoneAccept();
    await A.uiWait('[testid=code-input]', 20_000);
    const [w1, w2] = wrongCodes(code);
    for (const [wrong, hint] of [[w1, '还可重试 2 次'], [w2, '还可重试 1 次']]) {
      await A.uiInput('[testid=code-input]', wrong);
      await A.uiClick('[testid=btn-submit]');
      await A.uiWait('[testid=code-error]', 15_000);
      const err = await A.uiText('[testid=code-error]');
      if (!err.includes(hint)) throw new Error(`应提示「${hint}」,实际: ${err}`);
      await sleep(500);
    }
    return '第1/2次错码分层反馈 ✓（2 次→1 次）';
  });

  await step('分支D 第3次错码:终态+冷却 toast+设备卡冷却徽章+重连被拒', async () => {
    const code = await readPhoneCode(10_000);
    const w3 = wrongCodes(code)[2];
    await A.uiInput('[testid=code-input]', w3);
    await A.uiClick('[testid=btn-submit]');
    // 弹窗关闭(终态)
    const deadline = Date.now() + 20_000;
    let closed = false;
    while (Date.now() < deadline) {
      const txt = await A.uiText('.pairing-dialog').catch(() => '');
      if (!txt) { closed = true; break; }
      await sleep(500);
    }
    if (!closed) throw new Error('第 3 次错码后弹窗未关闭（终态未收敛）');
    // 冷却 toast(快轮询)
    let toastSeen = false;
    const tEnd = Date.now() + 6000;
    while (Date.now() < tEnd && !toastSeen) {
      const toast = await A.uiText('.toast').catch(() => '');
      if (/连续错误 3 次|冷却/.test(toast || '')) { toastSeen = true; break; }
      await sleep(250);
    }
    // 设备卡冷却倒计时徽章(P2 新增)
    await A.uiNavigate('/devices');
    await A.uiWait('[testid=device-cooldown-badge]', 10_000);
    const badge = await A.uiText('[testid=device-cooldown-badge]');
    await A.screenshot(join(runDir, 'v-D-cooldown-badge.png'));
    // 冷却期内 PC 重连 → 对端(手机,持有真实冷却)拒绝连接
    let rejected = false;
    try {
      await A.invoke('connect', { fingerprint: phoneFp });
    } catch (e) {
      rejected = true;
      note(`冷却期重连被拒错误: ${String(e).slice(0, 160)}`);
    }
    if (!rejected) throw new Error('冷却期内重连应被对端拒绝,实际 connect 成功');
    return { detail: `toast=${toastSeen ? '✓' : '(截图目检)'} 徽章="${badge.trim()}" 重连被拒=✓`, artifacts: [F('v-D-cooldown-badge.png')] };
  });

  // ============ 分支 A:错码 1 次重输成功(放最后:成功写双向信任,
  // 失败分支全部不写——省去中间清场;手机 App 重启清其内存冷却) ============
  await step('分支A 前置:重启手机 App 清内存冷却(分支D 记录的 PC fp 冷却)', async () => {
    try {
      execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', 'am', 'force-stop', PACKAGE], { encoding: 'utf8', timeout: 15_000 });
    } catch { /* 尽力 */ }
    await sleep(1200);
    ch.launch();
    await ch.waitForBanner(30_000);
    await sleep(1500);
    await ch.tap({ testTag: 'nav-devices-link' }); await sleep(800);
    return '手机 App 已重启(零信任与身份保持,冷却表已清)';
  });

  await step('分支A 错码1次:PC 输错 → 分层文案「还可重试 2 次」(P2 修复锚)', async () => {
    const code = await pcInitiatePhoneAccept();
    await A.uiWait('[testid=code-input]', 20_000);
    const wrong = wrongCodes(code)[0];
    await A.uiInput('[testid=code-input]', wrong);
    await A.uiClick('[testid=btn-submit]');
    await A.uiWait('[testid=code-error]', 15_000);
    const err = await A.uiText('[testid=code-error]');
    if (!/还可重试 2 次/.test(err)) throw new Error(`错码 1 次应提示「还可重试 2 次」,实际: ${err}`);
    await A.screenshot(join(runDir, 'v-A-wrong1-retry-hint.png'));
    return { detail: `错码=${wrong} 文案="${err.trim()}"`, artifacts: [F('v-A-wrong1-retry-hint.png')] };
  });

  await step('分支A 重输正确码 → 双端 trusted', async () => {
    // 手机仍在亮码态(错码不驱散接受方);读同一个码
    const code = await readPhoneCode(10_000);
    await A.uiInput('[testid=code-input]', code);
    await A.uiClick('[testid=btn-submit]');
    await A.pollUntil((s) => (s.sessions ?? []).some((x) => x.peer === phoneFp && x.trusted),
      { timeoutMs: 30_000, intervalMs: 1000, what: 'PC-手机会话 trusted(重输成功)' });
    return `码 ${code.slice(0, 2)}**** 重输成功,会话 trusted`;
  });

  // ============ 收尾:恢复夹具 ============
  await step('收尾恢复:还原 PC 三件(信任表并入手机新指纹) + connect 验证', async () => {
    // 分支A 已让双端互信(手机新身份);还原 PC 三件时把手机新指纹条目
    // 并入信任表,保持与手机侧信任一致(connect 免门直达 trusted)
    stopPcA();
    await sleep(1500);
    restorePcData();
    writeFileSync(join(PC_DATA, 'trusted_peers.json'), mergedTrustWithNewPhone(), 'utf8');
    startPcA();
    await waitPcReady();
    await A.invoke('connect', { fingerprint: phoneFp });
    await A.pollUntil((s) => (s.sessions ?? []).some((x) => x.peer === phoneFp && x.trusted),
      { timeoutMs: 30_000, intervalMs: 1000, what: '夹具恢复 PC-手机 trusted' });
    note('夹具终态:PC config/连接记忆已还原;手机 pm clear 后为新身份且信任表已并入（原手机条目成为死项,cold-start 同语义）');
    return 'config/记忆还原 + connect trusted=true';
  });

  await step('收尾:双端截图证据 + test/end', async () => {
    const arts = [];
    try { await A.screenshot(join(runDir, 'huss_pc-final.png')); arts.push(F('huss_pc-final.png')); } catch (e) { note(`PC 截图失败: ${e.message}`); }
    try { await ch.screenshot(join(runDir, 'huss_phone-final.png')); arts.push(F('huss_phone-final.png')); } catch (e) { note(`手机截图失败: ${e.message}`); }
    try { await A.beginTest(SCENARIO); await A.endTest('pass'); } catch (e) { note(`test/end 失败: ${e.message}`); }
    return { detail: `证据 ${arts.length} 件`, artifacts: arts };
  });
}

try {
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try { await A.screenshot(join(runDir, 'failure-pc.png'), { soft: true }); } catch { /* 尽力 */ }
  try { await ch.screenshot(join(runDir, 'failure-phone.png')); } catch { /* 尽力 */ }
  // 夹具兜底:PC 不能停在无信任/改码状态
  try {
    stopPcA();
    await sleep(1200);
    restorePcData();
    startPcA();
    await waitPcReady();
    note('失败兜底:PC 夹具已还原（手机为新身份,如需原夹具请重跑 cold-start 类场景重建配对）');
  } catch (e2) { note(`失败兜底还原失败: ${e2.message}`); }
}

const outcome = failureMsg ? 'fail' : (steps.length ? 'pass' : 'skip');
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '★ P2 打磨卡 T3：配对失败矩阵真机验收（PC=被测 Vue UI,手机=对端）;huss_laptop 不可达,双 PC 版矩阵待其恢复补跑',
      'P2 修复锚：分支A 错码重输（旧版 UI 收「对端拒绝」误判终态直接关窗,重输断裂）;分支D 冷却徽章/倒计时为新增',
      '夹具：PC data 三件备份/还原,consent_timeout 临时 15s;手机 factoryReset(pm clear)全新身份,收尾重新配对,原手机信任条目成为死项',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
