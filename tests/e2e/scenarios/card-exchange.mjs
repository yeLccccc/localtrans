// 场景：card-exchange（M3a FR3/FR4 名片体系，计划卡 T3）
//
// ★★ 状态：待 huss_laptop（192.168.0.222）恢复执行 —— 设备休眠/不可达时本场景
//    自动 SKIP 退出（exit 0 + 报告 outcome=skip）；已注册 run-all（注册表自动跳）。
//    首跑前需 node lib/deploy.mjs 重建双端（名片命令在 M3a T3 构建里）。★★
//
// 验收路径（spec 验收标准 1"名片跨网段互加"+ FR3/FR4）：
//   0) 双 PC 不可达（任一 bridge 未就绪）→ SKIP（条件跳过设计）。
//   1) 清场：A/B 双向 remove_trusted → 断言信任表空（配对从零开始）。
//   2) A 生成名片 get_business_card → 断言文本形态（表头/名称/64hex 指纹/
//      至少一条 地址:ip:port；不含 PSK/密钥字样）。
//   3) B 粘贴名片 add_by_card(A 名片) → 断言 {accepted:true, addresses_tried>=1}
//      （地址逐条入单播探测 + probe_targets 周期重探表）。
//   4) 名片自检：B add_by_card(B 自己的名片) → accepted=false 且不派发探测；
//      B add_by_card(垃圾文本) → 命令报错（解析 fail-closed）。
//   5) B 发现 A（单播探测回应，设备表出现 A 且在线）→ 配对：A connect 发起 →
//      B UI 同意门 → 读码 → A UI 输码 → 双端 sessions trusted——名片即入口，
//      无需广播先行可见。
//   6) 收尾恢复夹具：L3 keepIdentity+seedTrust 双向互播 + UI 设备名还原 + connect 验证。
//
// 隔离说明（与 spec 验收 1 的差异，真机联调时补）：双 PC 同处一路由器广播互通，
// 第 5 步"发现"无法排除广播路径贡献；本骨架先验证名片通路本身（生成/解析/
// 注入/探测/配对），广播隔离（防火墙挡广播需管理员，或 relay-path 式端口错开）
// 待 huss_laptop 恢复后联调定案——名片命令级断言（2/3/4 步）不受影响。
//
// 任何一步失败：collectEvidence → 报告 FAIL → 尽力兜底恢复 PC 夹具。
// 用法：node scenarios/card-exchange.mjs（前置：node lib/deploy.mjs 已部署双端）
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';

const SCENARIO = 'card-exchange';
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

/** 名片文本形态断言（命令级验收，不依赖隔离） */
function assertCardShape(text) {
  if (typeof text !== 'string' || !text.includes('LocalTrans 名片')) {
    throw new Error(`名片缺表头: ${JSON.stringify(text?.slice(0, 80))}`);
  }
  const fpLine = text.split('\n').find((l) => l.includes('指纹'));
  const fp = fpLine?.split(/[:：]/).slice(1).join(':').trim();
  if (!/^[0-9a-f]{64}$/.test(fp || '')) throw new Error(`指纹行非法: ${JSON.stringify(fpLine)}`);
  const addrs = text.split('\n').filter((l) => l.trim().startsWith('地址'));
  if (addrs.length < 1 || !addrs.every((l) => /^\s*地址\s*[:：]\s*[\d.[\]:a-fA-F]+:\d+\s*$/.test(l))) {
    throw new Error(`地址行缺失或非法: ${JSON.stringify(addrs)}`);
  }
  for (const secret of ['PSK', 'psk', 'PRIVATE KEY', '私钥', 'relay_psk']) {
    if (text.includes(secret)) throw new Error(`名片泄漏敏感字样: ${secret}`);
  }
  return { fp, addrCount: addrs.length };
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口：任一 PC 不可达 → SKIP（不等待设备恢复）----
  const aUp = await reachable(A);
  const bUp = aUp ? await reachable(B) : false;
  if (!aUp || !bUp) {
    const msg = `SKIP：${!aUp ? 'huss_pc' : 'huss_laptop'} 不可达（bridge 未就绪）——本场景待设备恢复后执行（M3a T3 部署期，huss_laptop 疑似休眠）`;
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
          'M3a 计划卡 T3 验收环境约束：huss_laptop（192.168.0.222）恢复后手动重跑本场景补双机证据',
          'core 侧名片生成/解析（roundtrip/容错/恶意输入 17 例）已由 cargo 单测覆盖，双机场景验证粘贴→发现→配对通路',
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
    return `huss_pc v${versions.huss_pc.appVersion} fp=${pcFp.slice(0, 8)}…，huss_laptop v${versions.huss_laptop.appVersion} fp=${laptopFp.slice(0, 8)}…`;
  });

  await step('双 PC test/begin', async () => {
    const a = await A.beginTest(SCENARIO);
    const b = await B.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…`;
  });

  await step('【清场】双向 remove_trusted → 断言信任表空（配对从零开始）', async () => {
    await A.invoke('remove_trusted', { fingerprint: laptopFp });
    await B.invoke('remove_trusted', { fingerprint: pcFp });
    for (const t of both) {
      const trusted = await t.invoke('list_trusted');
      if (trusted.length !== 0) throw new Error(`${t.name} 清场后信任表非空: ${JSON.stringify(trusted)}`);
    }
    return '双向信任已清（名片路径从零验证）';
  });

  await step('【生成】A get_business_card → 断言名片文本形态（表头/64hex 指纹/≥1 地址/无敏感字样）', async () => {
    const text = await A.invoke('get_business_card');
    const { fp, addrCount } = assertCardShape(text);
    if (fp !== pcFp) throw new Error(`名片指纹 (${fp.slice(0, 8)}…) ≠ 本机指纹 (${pcFp.slice(0, 8)}…)`);
    await A.screenshot(join(runDir, 'v-card-text.png'));
    return { detail: `指纹 ${fp.slice(0, 8)}… 匹配，地址 ${addrCount} 条（ip:quic_port 口径）`, artifacts: [F('v-card-text.png')] };
  });

  await step('【粘贴】B add_by_card(A 名片) → {accepted:true, addresses_tried>=1}', async () => {
    const text = await A.invoke('get_business_card');
    const r = await B.invoke('add_by_card', { text });
    if (r?.accepted !== true) throw new Error(`accepted 应为 true: ${JSON.stringify(r)}`);
    if (!(r.addresses_tried >= 1)) throw new Error(`addresses_tried 应 ≥1: ${JSON.stringify(r)}`);
    return `accepted=true, addresses_tried=${r.addresses_tried}（逐地址单播探测+入重探表）`;
  });

  await step('【名片自检】B 自己的名片 → accepted=false 不探测；垃圾文本 → 解析报错', async () => {
    const selfCard = await B.invoke('get_business_card');
    const self = await B.invoke('add_by_card', { text: selfCard });
    if (self?.accepted !== false || self?.addresses_tried !== 0) {
      throw new Error(`本机名片应拒绝且不探测: ${JSON.stringify(self)}`);
    }
    let rejected = false;
    try { await B.invoke('add_by_card', { text: '这是一段毫无名片结构的垃圾文本' }); }
    catch { rejected = true; }
    if (!rejected) throw new Error('垃圾文本应解析失败（fail-closed），却被接受');
    return '自卡拒绝 accepted=false/tried=0；垃圾文本拒绝——解析 fail-closed 双验证';
  });

  await step('【发现】B 设备表出现 A（单播探测回应）→ 配对：A connect → B 同意 → 输码 → 双端 trusted', async () => {
    await B.pollUntil((s) => s.devices.some((d) => d.id === pcFp && d.online),
      { timeoutMs: 30_000, what: 'huss_laptop 发现表出现 huss_pc（名片探测回应/广播可见）' });
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
    await B.screenshot(join(runDir, 'v-paired.png'));
    return { detail: `名片入口完成配对（码 ${code.slice(0, 2)}****）——双端 sessions trusted`, artifacts: [F('v-code-entry.png'), F('v-paired.png')] };
  });

  await step('收尾恢复夹具：L3 keepIdentity+seedTrust 互播 + 设备名还原 + connect 验证', async () => {
    await restorePcFixture();
    note(`夹具终态：huss_pc 信任=[huss_laptop]，huss_laptop 信任=[huss_pc]，设备名已还原，connect trusted=true`);
    return `L3 重置 + 互播 + connect trusted=true，设备名 huss_pc/huss_laptop`;
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
      '★ M3a T3：场景已落地、待 huss_laptop 恢复执行；设备不可达时条件跳过（outcome=skip, exit 0），run-all 注册表已接入',
      '名片地址口径=ip:quic_port（与发现表/直连同语义）；add_by_card 探测目标取其 IP+本机发现端口（ports 同源推导）',
      '双 PC 同广播域时第 5 步发现不排除广播路径贡献；广播隔离（防火墙挡广播需管理员/端口错开）待双机联调定案——命令级断言（名片形态/accepted/自卡拒绝/垃圾拒绝）不受影响',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
