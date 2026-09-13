// 场景：cold-start（T3，M1 计划卡）——零信任冷启动配对全链路（常态化 P0-6 教训）
//
// 三条零信任路径：
//   1) PC↔laptop 冷启动配对：L3 出厂重置双 PC（keepIdentity:false → 全新身份）→
//      互见 → A invoke connect 发起 → B UI 点 btn-grant → 读 B 屏 .code-display
//      （去空格取 \d{6}）→ A UI 输码提交 → 双端 sessions trusted + list_trusted 断言。
//      完成后 L3+seedTrust（keepIdentity:true 互播本轮新指纹）恢复信任夹具 +
//      connect 验证 + UI 恢复设备名（huss_pc/huss_laptop）。
//   2) PC↔phone 冷启动配对：手机 factoryReset（pm clear → 全新身份）→ banner 提取
//      新指纹 → PC discovery 发现（排除旧条目前缀）→ 手机设备页点 huss_pc 卡发起 →
//      PC UI 同意门 → 读码 → 手机输码屏（Compose Dialog 无 testTag：EditText 坐标
//      点击 + inputText + 「提交」文本按钮 tapXY）→ 断言 PC 会话 peer=手机新指纹。
//   3) 冷启动传输：PC→手机推 2MB（唯一戳内容）→ 手机 OfferSheet 点「接收」→
//      PC 卡终态 done + 字节断言；PC 共享区播种 3 小文件 → 手机文件页远程 tab 选
//      huss_pc → 浏览断言能列出文件（多选下载交互留 M2）。
//   收尾恢复夹具：手机保留新身份（指纹记 journal/报告）；PC 信任表 seedTrust 恢复
//   （huss_pc=[huss_laptop, 手机新指纹]，huss_laptop=[huss_pc]）+ connect 会话验证
//   + 设备名 UI 恢复。
//
// 注意：PC 身份每跑必然更换（冷启动语义，keepIdentity:false 烧掉旧身份）——
// 文档常量指纹（pc=7b00fc65…/laptop=5b36ef24…）只作运行前参考日志，场景内全部
// 动态取指纹；手机 pairing/OfferSheet 弹窗无 testTag（testTagsAsResourceId 只挂在
// NavHost，Dialog 独立 window 不继承）→ 按 className/text 定位。
//
// 任何一步失败：collectEvidence → 报告 FAIL → 尽力兜底恢复 PC 夹具（seedTrust+设备名）。
// 用法：node scenarios/cold-start.mjs   （前置：node lib/deploy.mjs 已部署双端）
import { execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync, readdirSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { createAdbChannel } from '../lib/adb.mjs';

const SCENARIO = 'cold-start';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const stamp = `${ts}-${rand}`;
const runId = `${SCENARIO}-${stamp}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);
const B = new Target('huss_laptop', targets.huss_laptop, journal);
const both = [A, B];
const ch = createAdbChannel({ serial: targets.huss_phone.serial });

// 文档参考指纹（仅预检日志；首轮后 PC 身份每跑更换，属预期）
const FP_DOC = {
  pc: '7b00fc65699ae45abd5b9a1c9b73721c8e29be53afb0bbf1be57aaec898bc0d8',
  laptop: '5b36ef248764f3005cfb878a8c0f2278cb412d3d78fe84390038f0e57f2eb28e',
};
const OLD_PHONE_FP_PREFIXES = ['42611bb9', '13bf68e8']; // PC 名册里的手机历史条目
const ALLOW_RE = /^(始终允许|仅在使用中允许|允许|仅本次允许)$/; // MIUI 权限弹窗（同 adb.mjs factoryReset 流程）

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
// 运行期状态（失败兜底恢复夹具用）
let pcFp = null;      // huss_pc 当前指纹（冷启动后为新身份）
let laptopFp = null;  // huss_laptop 当前指纹（同上）
let phoneFpNew = null; // 手机出厂重置后的新指纹（收尾 journal/报告记录）
let phoneNameNew = ''; // 手机出厂后广播名（ffi 首始默认「我的手机」）
let shareFiles = [];   // 冷启动3 播种进 PC 共享区的文件名
const phoneArtifacts = [];

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const digits = (s) => (s || '').replace(/\D/g, '');

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

/** 原生 adb shell（adb.mjs 通道未暴露通用 run，这里仅用于 IME 状态/keyevent，不改库） */
function adbShell(cmd) {
  return execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', cmd], {
    encoding: 'utf8', timeout: 15_000, windowsHide: true,
  });
}

async function imeShown() {
  try { return /mInputShown=true/.test(adbShell('dumpsys input_method')); }
  catch { return false; }
}

/** 手机 MIUI 权限弹窗顺手放行（与 adb.mjs factoryReset 同一文案清单）；
 * 命中返回 true（调用方轮询时先放行再继续找目标元素） */
async function dismissPermissionPopup() {
  try {
    const els = await ch.dump();
    const hit = els.find((e) => e.clickable && ALLOW_RE.test((e.text || '').trim()));
    if (hit) { await ch.tapXY(hit.center[0], hit.center[1]); await sleep(1000); return true; }
  } catch { /* dump 失败下轮重试 */ }
  return false;
}

const fingerprintOf = async (t) => (await t.invoke('get_device_fingerprint')).fingerprint_hex;

/** PC 侧读配对码（acceptor 亮码 .code-display，码可能带空格分组）；
 * 超时兜底白名单只读 get_pairing_pending */
async function readPcPairingCode(t, peerFp, { timeoutMs = 20_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastTxt = '';
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

/** PC↔PC connect（对端刚重启会话通道可能未就绪，短重试）+ 双端 sessions trusted 轮询 */
async function pcConnectVerify(fpSelf, fpPeer) {
  let lastErr = null;
  for (let i = 0; i < 3; i++) {
    try { await A.invoke('connect', { fingerprint: fpPeer }); lastErr = null; break; }
    catch (e) { lastErr = e; await sleep(3000); }
  }
  if (lastErr) throw lastErr;
  await Promise.all([
    A.pollUntil((s) => s.sessions.some((x) => x.peer === fpPeer && x.trusted),
      { timeoutMs: 30_000, what: 'huss_pc sessions trusted' }),
    B.pollUntil((s) => s.sessions.some((x) => x.peer === fpSelf && x.trusted),
      { timeoutMs: 30_000, what: 'huss_laptop sessions trusted' }),
  ]);
}

/** UI 恢复设备名（L3 后 config.json 被清 → 出厂名=COMPUTERNAME；
 * 手机侧展示/远程选卡按设备名匹配，夹具必须还原 huss_pc/huss_laptop） */
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

/** 手机设备页点 huss_pc 卡发起配对（主窗口 Compose 有 testTag；按当前指纹精确定位，
 * tap 元素中心坐标——文本节点可能不可点） */
async function phoneTapPcCard(pcFingerprint, { timeoutMs = 30_000 } = {}) {
  await ch.tap({ testTag: 'nav-devices-link' });
  await sleep(1500);
  const tag = `device-card-${pcFingerprint}`;
  const deadline = Date.now() + timeoutMs;
  let lastSummary = '';
  while (Date.now() < deadline) {
    if (await dismissPermissionPopup()) continue;
    try {
      const els = await ch.dump();
      const card = els.find((e) => e.testTag === tag);
      if (card) { await ch.tapXY(card.center[0], card.center[1]); return; }
      lastSummary = ch.dumpSummary(els, 18);
    } catch { /* dump 失败重试 */ }
    await sleep(1500);
  }
  throw new Error(`手机设备页未找到 ${tag}（PC 已发现手机、手机未发现 PC？）\n${lastSummary}`);
}

/** 手机输码屏（Compose Dialog 无 testTag）：EditText 坐标点击聚焦 → inputText →
 * 校验入框（残留先 DEL 清掉再补输）→ 收起 IME（防「提交」被键盘遮挡）→
 * 「提交」文本按钮 tapXY */
async function phoneEnterPairCode(code, { timeoutMs = 30_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let input = null;
  while (Date.now() < deadline) {
    if (await dismissPermissionPopup()) continue;
    try {
      const els = await ch.dump();
      input = els.find((e) => (e.className || '').includes('EditText'));
      if (input) break;
    } catch { /* 重试 */ }
    await sleep(1200);
  }
  if (!input) throw new Error('手机输码屏未出现（dump 无 EditText）');
  await ch.tapXY(input.center[0], input.center[1]);
  await sleep(800);
  ch.inputText(code); // 纯数字，adb input text 可注入
  await sleep(600);

  // 校验 6 位已入框（Compose TextField 语义 text=已输内容）；不符则清空重输
  let et = null;
  try {
    const els = await ch.dump();
    et = els.find((e) => (e.className || '').includes('EditText'));
  } catch { /* 按补输处理 */ }
  if (!et) throw new Error('输码后 EditText 消失（对话框可能被关闭）');
  if (digits(et.text) !== code) {
    await ch.tapXY(et.center[0], et.center[1]);
    await sleep(500);
    if ((et.text || '').length > 0) {
      for (let i = 0; i < 8; i++) adbShell('input keyevent 67'); // KEYCODE_DEL 清残留
      await sleep(400);
    }
    ch.inputText(code);
    await sleep(600);
    let ok = false;
    try {
      const els = await ch.dump();
      const et2 = els.find((e) => (e.className || '').includes('EditText'));
      ok = !!et2 && digits(et2.text) === code;
    } catch { /* 终检失败走下方抛错 */ }
    if (!ok) throw new Error(`手机输码框内容校验失败（期望 ${code}）`);
  }

  if (await imeShown()) { adbShell('input keyevent 4'); await sleep(800); } // BACK 只收 IME
  const els = await ch.dump();
  const submit = els.find((e) => e.text === '提交');
  if (!submit) throw new Error(`手机「提交」按钮未找到:\n${ch.dumpSummary(els, 20)}`);
  await ch.tapXY(submit.center[0], submit.center[1]);
}

/** 手机 OfferSheet 点「接收」（text==='接收' tapXY；弹窗有倒计时自动拒绝，
 * 轮询最多 25 轮 × 800ms；顺手放行 MIUI 杂弹） */
async function phoneAcceptOffer({ maxRounds = 25, intervalMs = 800 } = {}) {
  for (let i = 0; i < maxRounds; i++) {
    await sleep(intervalMs);
    if (await dismissPermissionPopup()) continue;
    let els;
    try { els = await ch.dump(); } catch { continue; }
    const btn = els.find((e) => e.text === '接收');
    if (btn) {
      await ch.tapXY(btn.center[0], btn.center[1]);
      return true;
    }
  }
  return false;
}

/** 夹具恢复：L3 keepIdentity+seedTrust（互播 + 手机新指纹）+ connect 验证 + 设备名。
 * 信任表目标态：huss_pc=[huss_laptop,手机]，huss_laptop=[huss_pc] */
async function restorePcFixture({ withPhone }) {
  const seed = {
    huss_pc: withPhone && phoneFpNew
      ? [{ fingerprint: laptopFp, name: 'huss_laptop' }, { fingerprint: phoneFpNew, name: 'huss_phone' }]
      : [{ fingerprint: laptopFp, name: 'huss_laptop' }],
    huss_laptop: [{ fingerprint: pcFp, name: 'huss_pc' }],
  };
  await reset(3, both, { keepIdentity: true, seedTrust: seed });
  await restoreDeviceName(A, 'huss_pc');
  await restoreDeviceName(B, 'huss_laptop');
  await pcConnectVerify(pcFp, laptopFp);
  for (const [t, want] of [[A, seed.huss_pc], [B, seed.huss_laptop]]) {
    const trusted = await t.invoke('list_trusted');
    const fps = trusted.map((p) => p.fingerprint).sort();
    const expect = want.map((e) => e.fingerprint).sort();
    if (JSON.stringify(fps) !== JSON.stringify(expect)) {
      throw new Error(`${t.name} 信任表不符: [${fps}] != [${expect}]`);
    }
  }
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  await step('环境握手：双 PC bridge 就绪 + 手机唤醒亮屏（记录预运行指纹）', async () => {
    for (const t of both) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      versions[t.name] = v;
    }
    ch.wake();
    ch.setStayOn(true);
    await sleep(1500); // 唤醒瞬间 Compose 树残缺，稳一下再 dump
    const fpA = await fingerprintOf(A);
    const fpB = await fingerprintOf(B);
    const drift = [fpA === FP_DOC.pc, fpB === FP_DOC.laptop];
    note(`预运行指纹 huss_pc=${fpA} huss_laptop=${fpB}`);
    return `huss_pc v${versions.huss_pc.appVersion} fp=${fpA.slice(0, 8)}…，huss_laptop v${versions.huss_laptop.appVersion} fp=${fpB.slice(0, 8)}…`
      + `（与文档常量一致=${drift.every(Boolean)}，冷启动场景每跑更换身份属预期）`;
  });

  await step('双 PC test/begin', async () => {
    const a = await A.beginTest(SCENARIO);
    const b = await B.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…`;
  });

  // ============ 路径 1：PC↔laptop 冷启动配对 ============

  await step('【冷启动1】L3 出厂重置双 PC（keepIdentity:false → 全新身份）', async () => {
    return await reset(3, both, { keepIdentity: false });
  });

  await step('【冷启动1】记录新身份 + 断言信任表空', async () => {
    const detail = [];
    for (const t of both) {
      const fp = await fingerprintOf(t);
      if (!/^[0-9a-f]{64}$/.test(fp)) throw new Error(`${t.name} 新指纹非 64hex: ${fp}`);
      const trusted = await t.invoke('list_trusted');
      if (trusted.length !== 0) throw new Error(`${t.name} L3 后信任表非空: ${JSON.stringify(trusted)}`);
      if (t === A) pcFp = fp; else laptopFp = fp;
      detail.push(`${t.name} fp=${fp.slice(0, 8)}…`);
    }
    note(`冷启动新身份 huss_pc=${pcFp} huss_laptop=${laptopFp}`);
    return detail.join('；') + '（信任表均空）';
  });

  await step('【冷启动1】等双端互见（新指纹）', async () => {
    await Promise.all([
      A.pollUntil((s) => s.devices.some((d) => d.id === laptopFp),
        { timeoutMs: 30_000, what: 'huss_pc 发现 huss_laptop 新指纹' }),
      B.pollUntil((s) => s.devices.some((d) => d.id === pcFp),
        { timeoutMs: 30_000, what: 'huss_laptop 发现 huss_pc 新指纹' }),
    ]);
    return `互见达成 ${pcFp.slice(0, 8)}…↔${laptopFp.slice(0, 8)}…`;
  });

  await step('【冷启动1】真实配对：A connect → B UI 同意 → 读码 → A UI 输码提交', async () => {
    await A.invoke('connect', { fingerprint: laptopFp });
    // 注：不给后端发 test/step——L3 重启后活动 run 已失效（与 l3-reset 同理，
    // 编排器 journal/report 为准，收尾统一 re-begin + end）
    await B.uiWait('[testid=pairing-dialog]', 15_000);
    await B.uiClick('[testid=btn-grant]');
    const code = await readPcPairingCode(B, pcFp);
    await A.screenshot(join(runDir, 'v-pc1-code-entry.png'));
    await A.uiWait('[testid=code-input]', 20_000);
    await A.uiInput('[testid=code-input]', code);
    await A.uiClick('[testid=btn-submit]');
    await Promise.all([
      A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc sessions 含 huss_laptop 新指纹且 trusted' }),
      B.pollUntil((s) => s.sessions.some((x) => x.peer === pcFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop sessions 含 huss_pc 新指纹且 trusted' }),
    ]);
    return { detail: `配对完成（码 ${code.slice(0, 2)}****，全新身份全流程：同意门+读码+输码）`, artifacts: [F('v-pc1-code-entry.png')] };
  });

  await step('【冷启动1】断言：双端信任表各 1 条指向对方', async () => {
    const detail = [];
    for (const [t, want] of [[A, laptopFp], [B, pcFp]]) {
      const trusted = await t.invoke('list_trusted');
      if (trusted.length !== 1 || trusted[0].fingerprint !== want) {
        throw new Error(`${t.name} 信任表异常: ${JSON.stringify(trusted)}`);
      }
      detail.push(`${t.name}→${want.slice(0, 8)}…`);
    }
    return detail.join('；');
  });

  await step('【冷启动1】夹具恢复：L3+seedTrust 互播新指纹 + connect 验证 + 设备名还原', async () => {
    await restorePcFixture({ withPhone: false });
    return `keepIdentity 保新身份，互播 ${pcFp.slice(0, 8)}…↔${laptopFp.slice(0, 8)}…，connect trusted=true，设备名已还原`;
  });

  // ============ 路径 2：PC↔phone 冷启动配对 ============

  await step('【冷启动2】手机 factoryReset（pm clear → 全新身份 → MIUI 权限放行）', async () => {
    const r = await ch.factoryReset();
    const m = r.banner.match(/fingerprint=([0-9a-f]{64})/);
    if (!m) throw new Error(`banner 未含指纹: ${r.banner}`);
    phoneFpNew = m[1];
    phoneNameNew = (r.banner.match(/device=(\S+)/) || [])[1] || '';
    note(`手机新指纹(出厂重置后)=${phoneFpNew}（旧身份 42611bb9… 已 pm clear 烧毁，新指纹记入 journal）`);
    return `pm clear ${r.cleared.trim()}，新指纹 ${phoneFpNew.slice(0, 8)}…（广播名 ${phoneNameNew || '?'}），权限放行 ${r.grants.length} 枚`;
  });

  await step('【冷启动2】PC discovery 发现手机新指纹（排除旧条目）', async () => {
    await A.pollUntil((s) => s.devices.some((d) => d.id === phoneFpNew && d.online),
      { timeoutMs: 45_000, what: `huss_pc devices 出现手机新指纹 ${phoneFpNew.slice(0, 8)}… 且 online` });
    const s = await A.state();
    const stale = s.devices.filter((d) => OLD_PHONE_FP_PREFIXES.some((p) => d.id.startsWith(p)));
    if (stale.length) note(`PC 名册残留手机旧条目 ${stale.length} 条（${stale.map((d) => d.id.slice(0, 8)).join(',')}…），以新指纹为准`);
    return `手机新指纹 ${phoneFpNew.slice(0, 8)}… online（旧条目 ${stale.length} 条仅存档不作数）`;
  });

  await step('【冷启动2】手机发起配对：点 huss_pc 卡 → PC UI 同意门 → 读码 → 手机输码', async () => {
    await phoneTapPcCard(pcFp);
    await A.uiWait('[testid=pairing-dialog]', 25_000);
    await A.uiClick('[testid=btn-grant]');
    const code = await readPcPairingCode(A, phoneFpNew);
    await A.screenshot(join(runDir, 'v-pc2-code.png'));
    await phoneEnterPairCode(code);
    await A.pollUntil((s) => s.sessions.some((x) => x.peer === phoneFpNew && x.trusted),
      { timeoutMs: 30_000, what: 'huss_pc sessions 含手机新指纹且 trusted' });
    // 手机侧成功弹窗（证据非硬断言：SuccessDialog 出现即拍；出现则点「确定」关掉）
    try {
      const els = await ch.dump();
      if (els.some((e) => e.text === '配对成功')) {
        phoneArtifacts.push(F('v-phone2-pair-success.png'));
        await ch.screenshot(join(runDir, 'v-phone2-pair-success.png'));
        const ok = els.find((e) => e.text === '确定');
        if (ok) await ch.tapXY(ok.center[0], ok.center[1]);
      }
    } catch { /* 证据尽力而为 */ }
    return {
      detail: `配对完成（码 ${code.slice(0, 2)}****，手机 EditText 输码 + 「提交」）`,
      artifacts: [F('v-pc2-code.png'), ...phoneArtifacts.filter((a) => a.includes('pair-success'))],
    };
  });

  await step('【冷启动2】断言：PC 会话 peer=手机新指纹 且 trusted + 信任表含手机', async () => {
    const s = await A.state();
    const sess = s.sessions.find((x) => x.peer === phoneFpNew);
    if (!sess) throw new Error(`huss_pc sessions 无手机新指纹: ${JSON.stringify(s.sessions)}`);
    if (!sess.trusted) throw new Error('huss_pc 手机会话 trusted=false');
    const trusted = await A.invoke('list_trusted');
    if (!trusted.some((p) => p.fingerprint === phoneFpNew)) {
      throw new Error(`huss_pc 信任表缺手机新指纹: ${JSON.stringify(trusted.map((p) => p.fingerprint))}`);
    }
    return `sessions peer=${phoneFpNew.slice(0, 8)}… trusted=true；信任表 ${trusted.length} 条含手机`;
  });

  // ============ 路径 3：冷启动传输 ============

  await step('【冷启动3】PC→手机推送 2MB（唯一戳）→ 手机 OfferSheet 点「接收」', async () => {
    const pushName = `cs-push-${rand}.bin`;
    const pushPath = join(runDir, pushName);
    const buf = Buffer.alloc(2 * 1024 * 1024);
    buf.write(`COLD-START PUSH ${runId}`, 0, 'utf8');
    buf.fill(0x5a, 64);
    writeFileSync(pushPath, buf);
    const cardId = await A.invoke('push_files', { fingerprint: phoneFpNew, local_paths: [pushPath] });
    const accepted = await phoneAcceptOffer();
    if (!accepted) throw new Error('手机 25 轮内未出现「接收」弹窗（OfferSheet）');
    await ch.screenshot(join(runDir, 'v-phone3-offer-accepted.png'));
    phoneArtifacts.push(F('v-phone3-offer-accepted.png'));
    const done = await A.pollUntil((st) => {
      const t = (st.transfers || []).find((x) => x.id === cardId);
      return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
    }, { timeoutMs: 120_000, intervalMs: 1000, what: '推送卡终态' });
    const card = done.value;
    if (card.state !== 'done') throw new Error(`推送卡 state=${card.state} failReason=${card.failReason}`);
    if (card.bytesDone !== card.bytesTotal) throw new Error(`字节断言: done(${card.bytesDone}) != total(${card.bytesTotal})`);
    if (card.bytesTotal !== 2 * 1024 * 1024) throw new Error(`total=${card.bytesTotal} != 2MB`);
    await A.screenshot(join(runDir, 'v-pc3-push-done.png'));
    return {
      detail: `卡 ${cardId.slice(0, 8)}… done 2097152B/2097152B，手机已点「接收」`,
      artifacts: [F('v-pc3-push-done.png'), F('v-phone3-offer-accepted.png')],
    };
  });

  await step('【冷启动3】PC 共享区播种 3 个小文件（唯一戳）', async () => {
    const settings = await A.invoke('get_settings');
    const sharePath = settings.shares?.[0]?.path;
    if (!sharePath) throw new Error(`huss_pc 无共享区: ${JSON.stringify(settings.shares)}`);
    // 清历史 run 的 cs-* 播种残留（多次运行累积会淹远程列表/触发分页，
    // 2026-09-07 实证：12 个残留文件致手机端列表断言失败）
    for (const f of readdirSync(sharePath)) {
      if (/^cs-[abc]-[a-z0-9]+\.txt$/.test(f)) { try { unlinkSync(join(sharePath, f)); } catch { /* 尽力 */ } }
    }
    shareFiles = ['cs-a', 'cs-b', 'cs-c'].map((n) => `${n}-${rand}.txt`);
    for (const n of shareFiles) {
      writeFileSync(join(sharePath, n), Buffer.from(`cold-start share ${runId} ${n}\n`, 'utf8'));
    }
    return `3 文件 → ${sharePath}（${shareFiles.join(', ')}）`;
  });

  await step('【冷启动3】手机远程浏览 huss_pc 共享区 → 断言能列出文件', async () => {
    const pcName = (await A.invoke('get_settings')).device_name;
    await ch.tap({ testTag: 'nav-files-link' });
    await sleep(1500);
    await ch.tap({ testTag: 'files-location-remote-tab' });
    await sleep(2000);
    // 浏览断言：dump 只读（单击远程文件会直拉下载，不点文件条目）。
    // 已知产品首入竞态：setSelectedDevice 时 loadEntries 先于 SharesResp 执行
    // （shareId 尚空 → 条目空且不重载）→ 表现为选中后内容空白；
    // 恢复法=本机/远程 tab 互换重进（ shares 已缓存，二次选择即列出）。
    // 「对方尚未共享文件夹」态=SharesReq 整体失败 → 换个设备 回选择页重选。
    const deadline = Date.now() + 90_000;
    const found = new Set();
    let lastDump = [];
    let toggles = 0;
    while (Date.now() < deadline && found.size < shareFiles.length) {
      if (await dismissPermissionPopup()) continue;
      try { lastDump = await ch.dump(); } catch { await sleep(1200); continue; }
      for (const n of shareFiles) {
        if (lastDump.some((e) => (e.text || '').includes(n))) found.add(n);
      }
      if (found.size === shareFiles.length) break;
      const back = lastDump.find((e) => e.text === '换个设备');
      const card = lastDump.find((e) => e.text === pcName || (e.text || '').includes('huss_pc'));
      if (back) {
        await ch.tapXY(back.center[0], back.center[1]);
        await sleep(2000);
      } else if (card) {
        await ch.tapXY(card.center[0], card.center[1]);
        await sleep(3000);
      } else {
        // 选中后空白态 → tab 互换重进（重 compose 远程设备选择页）
        if (toggles++ > 8) break;
        await ch.tap({ testTag: 'files-location-local-tab' });
        await sleep(1300);
        await ch.tap({ testTag: 'files-location-remote-tab' });
        await sleep(2200);
      }
    }
    await ch.screenshot(join(runDir, 'v-phone3-remote-list.png'));
    phoneArtifacts.push(F('v-phone3-remote-list.png'));
    if (found.size !== shareFiles.length) {
      throw new Error(`远程浏览仅列出 ${found.size}/${shareFiles.length} 个文件（重选 ${toggles} 次）:\n${ch.dumpSummary(lastDump, 20)}`);
    }
    return {
      detail: `远程 tab → ${pcName} → 列出 [${[...found].join(', ')}]（重选 ${toggles} 次；多选下载留 M2）`,
      artifacts: [F('v-phone3-remote-list.png')],
    };
  });

  // ============ 收尾恢复夹具 ============

  await step('收尾恢复夹具：seedTrust(huss_pc=[laptop,手机新指纹]) + connect 验证 + 设备名', async () => {
    await restorePcFixture({ withPhone: true });
    note(`夹具终态：huss_pc 信任=[huss_laptop, huss_phone(${phoneFpNew.slice(0, 8)}…)]，huss_laptop 信任=[huss_pc]，设备名已还原；手机保留新身份 ${phoneFpNew}`);
    return `PC 信任表恢复（含手机新指纹 ${phoneFpNew.slice(0, 8)}…），双 PC connect trusted=true，设备名 huss_pc/huss_laptop`;
  });

  await step('收尾：证据 + test/end', async () => {
    const arts = [...phoneArtifacts];
    for (const t of both) {
      // huss_laptop 显示器离位时截图 500 为存量环境问题——证据非断言
      try {
        await t.screenshot(join(runDir, `${t.name}-final.png`));
        arts.push(F(`${t.name}-final.png`));
      } catch (e) { note(`${t.name} 收尾截图失败（证据缺口，非断言）: ${e.message}`); }
    }
    const ev = await collectEvidence(both, runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    // 多轮重启 run 不连续：重新 begin 再 end（给后端完整结论），失败仅记笔记
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
    try { await ch.screenshot(join(runDir, 'v-fail-phone.png')); } catch { /* 尽力 */ }
    const ev = await collectEvidence(both, runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
    void ev;
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  try { await B.endTest('fail'); } catch { /* 尽力 */ }
  // 夹具兜底：PC 不能停在出厂/半配对态（手机侧无力回滚，保留当前身份）
  if (pcFp && laptopFp) {
    console.log(`[${SCENARIO}] 失败兜底：恢复 PC 信任夹具…`);
    try {
      await restorePcFixture({ withPhone: Boolean(phoneFpNew) });
      note('失败兜底夹具恢复完成');
    } catch (e3) { note(`失败兜底夹具恢复失败: ${e3.message}`); console.error(`[fixture] 兜底失败: ${e3.message}`); }
  }
}

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      `手机新指纹（factoryReset 后，已记 journal）：${phoneFpNew || '（未跑到）'}；手机保留新身份收场`,
      'PC 身份每跑必然更换（冷启动语义：keepIdentity:false 烧掉旧身份）——文档常量指纹 pc=7b00fc65…/laptop=5b36ef24… 仅首轮有效，场景内全部动态取指纹',
      '夹具恢复=L3 keepIdentity:true（保当轮新身份）+ seedTrust 互播（huss_pc=[huss_laptop,手机新指纹]）+ UI 恢复设备名（L3 后出厂名=COMPUTERNAME，手机侧按设备名匹配必须还原）',
      '配对确认/输码走真实 UI（btn-grant/.code-display/code-input）；connect/push_files 为白名单语义注入',
      '手机 pairing 输码屏与 OfferSheet 无 testTag：Compose Dialog 独立 window 不继承 NavHost 的 testTagsAsResourceId → EditText className 定位 + inputText + 「提交」text tapXY；提交前经 dumpsys input_method 收起 IME 防遮挡',
      '手机出厂后广播名为「我的手机」（ffi 首始默认），PC 名册识别以 banner 新指纹为准，旧条目（42611bb9/13bf68e8 开头）仅存档',
      '远程浏览只断言文件列表（dump 只读，单击远程文件会直拉下载故不点文件）；多选下载交互留 M2',
      '产品首入竞态（已知，留 M2/产品泳道）：远程首选设备时 loadEntries 先于 SharesResp 执行（shareId 尚空）→ 条目空白且不重载；场景以 本机/远程 tab 互换重选 自愈，重选后 shares 命中缓存即正常列出',
      '并发警示：relay-path 等场景若以 LOCALTRANS_TEST_PORT_BASE 起第二实例，与主实例共用 data/（身份/config/信任同名文件）——本场景首跑浏览失败即其干扰（名册地址被抢）；AGENTS.md「本机同时只跑一个 relay 泳道」须严格遵守',
      '场景自身多次 L3 重启双 PC（冷启动1×2 + 收尾×1），全程约 4-6 分钟属预期',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
