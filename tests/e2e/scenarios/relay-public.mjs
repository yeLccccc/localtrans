// 场景：relay-public（公网中继验收——relay.example.com 部署的 v0.13.0 localtrans-relay）
//
// ★★ 手动场景，未注册 run-all：需要外部公网中继 + LT_RELAY_PSK 环境变量 ★★
// 用法：LT_RELAY_PSK='<服务器 /etc/localtrans-relay.toml 的 psk>' \
//       node scenarios/relay-public.mjs
// 前置：双端已 node lib/deploy.mjs 部署运行；relay.example.com 中继已启动（systemd
//       localtrans-relay，控制面 udp/9443，数据面 udp/9000-9100）。
//
// 与 relay-path 的差异：中继不落在 huss_laptop，而是外部公网服务器——
//   - 双端均以 relay.example.com:9443 配置（设置页 UI 真实保存，同款 enableRelayViaUI）
//   - 隔离仍走端口基址错开（restartIsolated），本地发现机制上不可达，
//     唯一会话路径=公网中继（家庭 NAT → 阿里云 → 家庭 NAT，真跨网）
//   - 名册租约地址断言 host=服务器 public_ip、port∈数据面池
//   - 服务器侧 Punch/KNOCK 日志证据由运维通道（ssh）事后归档进报告目录，
//     场景内不携带服务器凭据
//
// 验收路径：
//   1) 双端握手 + 指纹采集
//   2) 隔离重启（端口基址错开）→ 8s 互不可见（直连消除）
//   3) 双端 UI 配置 relay.example.com:9443 + PSK → relay_status.connected（PSK 认证）
//   4) 名册互见 viaRelay=true + addr=203.0.113.10:<数据面池端口>
//   5) connect 建会话 → 双端 trusted + viaRelay 保持
//   6) 经公网推 5MB（唯一戳）→ 双端 done + 字节核对 + 非秒传
//   7) 收尾证据 + finally 清理（关 relay → 恢复端口 → L2 → L1）
import { mkdirSync, writeFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { PORT_BASE, restartIsolated, restoreAppPorts } from '../lib/relay.mjs';

// ---- 公网中继拓扑（凭据只从环境来，不落 git）----
// 注意：validate_relay_config 目前只接受 IP:端口（SocketAddr 字面量），
// 域名（relay.example.com:9443）保存会被拒——域名支持记打磨池，此处用服务器静态公网 IP。
const RELAY_HOST = process.env.LT_RELAY_ADDR || '203.0.113.10:9443';
const RELAY_PUBLIC_IP = process.env.LT_RELAY_PUBLIC_IP || '203.0.113.10';
const RELAY_PSK = process.env.LT_RELAY_PSK;
const DATA_START = 9000;
const DATA_END = 9100;
if (!RELAY_PSK) {
  console.error('缺少 LT_RELAY_PSK（服务器 /etc/localtrans-relay.toml 的 psk）');
  process.exit(1);
}

const SCENARIO = 'relay-public';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
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
let fixtureFile = null;
let expectedBytes = 0;
let fpA = null;
let fpB = null;
let savedPorts = null;

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

async function pollInvoke(t, cmd, pred, { timeoutMs = 20_000, intervalMs = 500, what = cmd } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last;
  for (;;) {
    last = await t.invoke(cmd);
    const hit = pred(last);
    if (hit) return hit;
    if (Date.now() >= deadline) {
      throw new Error(`${t.name} ${cmd} 轮询超时（${timeoutMs}ms）: ${what}，末值=${JSON.stringify(last)}`);
    }
    await sleep(intervalMs);
  }
}

const peerEntry = (snap, peerFp) => snap.devices.find((d) => d.id === peerFp);

// 与 relay-path 同款（真实用户流程：输入→开关→保存；防回填踩值与 @change 竞态）
async function enableRelayViaUI(t, server, psk) {
  const SAVE_BTN = '[testid=settings-relay-save-btn]';
  const SERVER_IN = '[testid=settings-relay-server-input]';
  const PSK_IN = '[testid=settings-relay-psk-input]';
  const TOGGLE = '[testid=settings-relay-enabled-toggle]';
  for (let attempt = 1; attempt <= 3; attempt++) {
    await t.uiNavigate('/transfers');
    await t.uiNavigate('/settings');
    await t.uiWait(SERVER_IN, 10_000);
    const cfgBefore = await t.invoke('get_settings');
    if (cfgBefore.relay_psk && !String(cfgBefore.relay_psk).startsWith('****')) {
      throw new Error(`${t.name} get_settings 回显明文 psk（A4 隐私契约破坏）`);
    }
    let loaded = false;
    for (let i = 0; i < 30 && !loaded; i++) {
      const tree = await t.uiTree();
      const nameEl = tree.find((e) => e.testId === 'settings-device-name-input');
      loaded = !!nameEl && nameEl.value === cfgBefore.device_name && String(nameEl.value).length > 0;
      if (!loaded) await sleep(300);
    }
    if (!loaded) throw new Error(`${t.name} 设置页配置回填超时（设备名输入框未回填 device_name）`);
    await t.uiInput(SERVER_IN, server, { events: ['blur'] });
    await t.uiInput(PSK_IN, psk, { events: ['blur'] });
    if (!cfgBefore.relay_enabled) {
      await t.uiClick(TOGGLE);
    }
    await sleep(300);
    await t.uiClick(SAVE_BTN);
    for (let i = 0; i < 10; i++) {
      const cfg = await t.invoke('get_settings');
      if (cfg.relay_enabled === true && cfg.relay_server === server) return `enabled, server=${cfg.relay_server}`;
      await sleep(500);
    }
    console.log(`  WARN  ${t.name} 中继保存未生效（第 ${attempt} 次），重试`);
  }
  throw new Error(`${t.name} 中继配置三次保存均未生效（get_settings 不匹配）`);
}

async function disableRelayViaUI(t) {
  const SAVE_BTN = '[testid=settings-relay-save-btn]';
  const SERVER_IN = '[testid=settings-relay-server-input]';
  const PSK_IN = '[testid=settings-relay-psk-input]';
  const TOGGLE = '[testid=settings-relay-enabled-toggle]';
  for (let attempt = 1; attempt <= 3; attempt++) {
    await t.uiNavigate('/transfers');
    await t.uiNavigate('/settings');
    await t.uiWait(SERVER_IN, 10_000);
    const cfg = await t.invoke('get_settings');
    let loaded = false;
    for (let i = 0; i < 30 && !loaded; i++) {
      const tree = await t.uiTree();
      const nameEl = tree.find((e) => e.testId === 'settings-device-name-input');
      loaded = !!nameEl && nameEl.value === cfg.device_name && String(nameEl.value).length > 0;
      if (!loaded) await sleep(300);
    }
    if (!loaded) throw new Error(`${t.name} 设置页配置回填超时`);
    if (cfg.relay_enabled) {
      await t.uiClick(TOGGLE);
      await sleep(300);
    }
    await t.uiInput(SERVER_IN, '', { clear: true, events: ['blur'] });
    await t.uiInput(PSK_IN, '', { clear: true, events: ['blur'] });
    await t.uiClick(SAVE_BTN);
    for (let i = 0; i < 10; i++) {
      const after = await t.invoke('get_settings');
      if (after.relay_enabled === false) return '已停用并清空';
      await sleep(500);
    }
    console.log(`  WARN  ${t.name} 中继停用未生效（第 ${attempt} 次），重试`);
  }
  throw new Error(`${t.name} 中继停用未生效`);
}

function makeFixture() {
  const dir = join(e2eRoot(), 'fixtures', 'runs', runId);
  mkdirSync(dir, { recursive: true });
  const size = 5 * 1024 * 1024;
  const buf = Buffer.allocUnsafe(size);
  let x = (0x4c54 ^ size) >>> 0;
  for (let i = 0; i < size; i++) {
    x ^= x << 13; x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5; x >>>= 0;
    buf[i] = x & 0xff;
  }
  const stamp = Buffer.from(`\n[relay-public ${runId}]\n`, 'utf8');
  const p = join(dir, `relay-public-5mb-${rand}.bin`);
  writeFileSync(p, Buffer.concat([buf, stamp]));
  return p;
}

async function cleanup() {
  console.log('\n[relay-public] 清理：UI 关 relay → 恢复端口 → L2 回默认 → L1 清卡');
  for (const t of [huss_pc, huss_laptop]) {
    try { note(`${t.name} 清理: ${await disableRelayViaUI(t)}`); }
    catch (e) { note(`${t.name} 清理关 relay 失败: ${e.message}`); }
  }
  if (savedPorts) {
    try { note(`恢复 config.json 原端口: ${await restoreAppPorts(savedPorts, targets)}`); savedPorts = null; }
    catch (e) { note(`恢复端口失败: ${e.message}`); }
  }
  try { note(await reset(2, [huss_pc, huss_laptop])); }
  catch (e) { note(`清理 L2 失败: ${e.message}`); }
  try { note(await reset(1, [huss_pc, huss_laptop])); }
  catch (e) { note(`清理 L1 失败: ${e.message}`); }
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}  relay=${RELAY_HOST}`);

  await step('环境握手：双端 health/version + 指纹采集', async () => {
    for (const t of [huss_pc, huss_laptop]) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪（先 node lib/deploy.mjs）`);
      if (v.apiVersion !== 1) throw new Error(`${t.name} apiVersion=${v.apiVersion}`);
      versions[t.name] = v;
    }
    const sA = await huss_pc.state();
    const sB = await huss_laptop.state();
    fpA = sA.selfDevice.id;
    fpB = sB.selfDevice.id;
    if (fpA?.length !== 64 || fpB?.length !== 64) throw new Error(`指纹异常: ${fpA} / ${fpB}`);
    return `fpA=${fpA.slice(0, 8)}… fpB=${fpB.slice(0, 8)}…`;
  });

  await step(`隔离重启：双端端口基址 pc=${PORT_BASE.huss_pc} / laptop=${PORT_BASE.huss_laptop}`, async () => {
    const { saved, patched } = await restartIsolated(targets);
    savedPorts = saved;
    const a = await huss_pc.beginTest(SCENARIO);
    const b = await huss_laptop.beginTest(SCENARIO);
    return `runA=${a.slice(0, 8)}… runB=${b.slice(0, 8)}…；config.json 端口补丁 ${patched}（广播/监听端口无交集）`;
  });

  await step('隔离断言：8s 窗口内双端互相不可见（直连路径消除）', async () => {
    const deadline = Date.now() + 8_000;
    let polls = 0;
    while (Date.now() < deadline) {
      const sA = await huss_pc.state();
      const sB = await huss_laptop.state();
      const aSeesB = peerEntry(sA, fpB);
      const bSeesA = peerEntry(sB, fpA);
      const aLocal = aSeesB && !(aSeesB.viaRelay === false && aSeesB.addr === '');
      const bLocal = bSeesA && !(bSeesA.viaRelay === false && bSeesA.addr === '');
      if (aLocal || bLocal) {
        throw new Error(`直连未隔离: pc见laptop=${JSON.stringify(aSeesB)} laptop见pc=${JSON.stringify(bSeesA)}`);
      }
      polls++;
      await sleep(1_500);
    }
    return `${polls} 次轮询均无本地发现/名册条目（仅信任兜底 offline 卡）`;
  });

  await step(`双端启用公网中继：设置页 UI 真实保存 ${RELAY_HOST}`, async () => {
    const r1 = await enableRelayViaUI(huss_pc, RELAY_HOST, RELAY_PSK);
    const r2 = await enableRelayViaUI(huss_laptop, RELAY_HOST, RELAY_PSK);
    return `huss_pc→${RELAY_HOST}（${r1}）；huss_laptop→${RELAY_HOST}（${r2}）`;
  });

  await step('名册可见：relay_status connected + 对端 viaRelay=true + 公网租约地址', async () => {
    await Promise.all([
      pollInvoke(huss_pc, 'relay_status', (r) => r.connected === true && r.devices >= 1,
        { timeoutMs: 30_000, what: 'relay connected + 名册含对端' }),
      pollInvoke(huss_laptop, 'relay_status', (r) => r.connected === true && r.devices >= 1,
        { timeoutMs: 30_000, what: 'relay connected + 名册含对端' }),
    ]);
    const check = async (t, peerFp, peerName) => {
      const d = await t.pollUntil(
        (s) => {
          const e = peerEntry(s, peerFp);
          return e && e.viaRelay === true ? e : undefined;
        },
        { timeoutMs: 20_000, what: `${t.name} 名册含 ${peerName}（viaRelay=true）` },
      );
      const [host, portStr] = d.value.addr.split(':');
      const port = Number(portStr);
      if (host !== RELAY_PUBLIC_IP) throw new Error(`${t.name} 视角对端 addr 主机非公网中继 IP: ${d.value.addr}`);
      if (!(port >= DATA_START && port <= DATA_END)) {
        throw new Error(`${t.name} 视角对端租约端口 ${port} 不在数据面池 ${DATA_START}-${DATA_END}: ${d.value.addr}`);
      }
      if (!d.value.online) throw new Error(`${t.name} 视角对端 online=false: ${JSON.stringify(d.value)}`);
      return d.value.addr;
    };
    const addrOnPc = await check(huss_pc, fpB, 'huss_laptop');
    const addrOnLaptop = await check(huss_laptop, fpA, 'huss_pc');
    return `pc 视角 laptop@${addrOnPc}；laptop 视角 pc@${addrOnLaptop}（均为公网租约地址）`;
  });

  await step('connect 建会话：huss_pc 发起 → 双端 sessions trusted（经公网中继 punch）', async () => {
    await huss_pc.invoke('connect', { fingerprint: fpB });
    await Promise.all([
      huss_pc.pollUntil(
        (s) => s.sessions.find((x) => x.peer === fpB && x.trusted),
        { timeoutMs: 45_000, what: 'huss_pc sessions 含 laptop 且 trusted' },
      ),
      huss_laptop.pollUntil(
        (s) => s.sessions.find((x) => x.peer === fpA && x.trusted),
        { timeoutMs: 45_000, what: 'huss_laptop sessions 含 pc 且 trusted' },
      ),
    ]);
    const sA = await huss_pc.state();
    const d = peerEntry(sA, fpB);
    if (!d?.viaRelay) throw new Error(`会话后 pc 视角 viaRelay 翻转: ${JSON.stringify(d)}`);
    const sB = await huss_laptop.state();
    if (!peerEntry(sB, fpA)?.viaRelay) throw new Error('会话后 laptop 视角 viaRelay=false');
    return '双端 sessions trusted=true，viaRelay 保持 true（跨网会话经中继建立）';
  });

  await step('经公网中继推 5MB：push_files（唯一戳）→ laptop UI 接收', async () => {
    await huss_pc.step('push-files');
    await huss_laptop.step('offer-accept');
    fixtureFile = makeFixture();
    expectedBytes = statSync(fixtureFile).size;
    const card = await huss_pc.invoke('push_files', { fingerprint: fpB, local_paths: [fixtureFile] });
    note(`push_files 占位卡 ${card}，文件 ${fixtureFile}（${expectedBytes}B）`);
    await huss_laptop.uiWait('.offer-modal', 30_000);
    await huss_laptop.uiClick('.offer-modal .btn-success');
    return `relay-public-5mb-${rand}.bin（${expectedBytes}B）已推送，laptop 已点接收`;
  });

  await step('断言：双端终态 done + 字节核对 + 非秒传 + viaRelay 复查', async () => {
    const fname = `relay-public-5mb-${rand}.bin`;
    const aPoll = await huss_pc.pollUntil(
      (s) => {
        const mine = s.transfers.filter((t) => t.direction === 'push' && t.name.includes(fname));
        return mine.length > 0 && mine.every((t) => t.state === 'done') ? mine : undefined;
      },
      { timeoutMs: 180_000, intervalMs: 500, what: 'huss_pc push 卡全部 done' },
    );
    const aCard = (aPoll.value.find((c) => c.bytesTotal === expectedBytes) || aPoll.value[0]);
    if (aCard.bytesDone !== aCard.bytesTotal) throw new Error(`pc bytesDone(${aCard.bytesDone}) != total(${aCard.bytesTotal})`);
    if (aCard.failReason) throw new Error(`pc 卡失败: ${aCard.failReason}`);
    if (aCard.instant) throw new Error('pc 卡 instant=true（秒传命中，唯一戳失效）');
    let senderNote = '';
    if (aCard.bytesTotal !== expectedBytes) {
      if (aCard.bytesTotal % expectedBytes === 0) {
        senderNote = `（发送端 total=${aCard.bytesTotal}=${aCard.bytesTotal / expectedBytes}×文件，命中 e2e-harness §8 done×2 已知bug，接收端字节为准）`;
        note(`发送端字节记账异常: ${senderNote}`);
      } else {
        throw new Error(`pc total ${aCard.bytesTotal} != 文件 ${expectedBytes}（非整数倍，非已知bug模式）`);
      }
    }

    const bPoll = await huss_laptop.pollUntil(
      (s) => {
        const mine = s.transfers.filter((t) => t.direction === 'pull' && t.name.includes(fname));
        return mine.length > 0 && mine.every((t) => t.state === 'done') ? mine : undefined;
      },
      { timeoutMs: 180_000, intervalMs: 500, what: 'huss_laptop pull 卡全部 done' },
    );
    const bCard = (bPoll.value.find((c) => c.bytesTotal === expectedBytes) || bPoll.value[0]);
    if (bCard.bytesDone !== bCard.bytesTotal) throw new Error(`laptop bytesDone(${bCard.bytesDone}) != total(${bCard.bytesTotal})`);
    if (bCard.bytesTotal !== expectedBytes) throw new Error(`laptop total ${bCard.bytesTotal} != 文件 ${expectedBytes}`);
    if (bCard.failReason) throw new Error(`laptop 卡失败: ${bCard.failReason}`);
    if (bCard.instant) throw new Error('laptop 卡 instant=true（秒传命中，唯一戳失效）');

    const sA = await huss_pc.state();
    const sB = await huss_laptop.state();
    if (!peerEntry(sA, fpB)?.viaRelay || !peerEntry(sB, fpA)?.viaRelay) {
      throw new Error('传输后 viaRelay 复查失败（设备视图回落本地语义）');
    }
    return `pc 卡=${aCard.id}、laptop 卡=${bCard.id}：双端 done、instant=false、viaRelay=true；接收端 ${bCard.bytesTotal}B 精确核对${senderNote}`;
  });

  await step('收尾：截图 + 证据 + test/end', async () => {
    const arts = [];
    const degraded = [];
    for (const t of [huss_pc, huss_laptop]) {
      try {
        await t.screenshot(join(runDir, `${t.name}-final.png`));
        arts.push(F(`${t.name}-final.png`));
      } catch (e) {
        degraded.push(`${t.name}: ${e.message}`);
        note(`${t.name} 截图失败（证据降级）: ${e.message}`);
      }
    }
    const ev = await collectEvidence([huss_pc, huss_laptop], runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    await huss_pc.endTest('pass');
    await huss_laptop.endTest('pass');
    return { detail: `证据 ${arts.length} 件落盘${degraded.length ? `（截图降级: ${degraded.join('; ')}）` : ''}`, artifacts: arts };
  });
}

// ---------------------------------------------------------------------------
try {
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try {
    await collectEvidence([huss_pc, huss_laptop], runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await huss_pc.endTest('fail'); } catch { /* 尽力 */ }
  try { await huss_laptop.endTest('fail'); } catch { /* 尽力 */ }
}

try { await cleanup(); } catch (e) { note(`清理异常: ${e.message}`); }

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      `拓扑：中继为外部公网服务器 ${RELAY_HOST}（public_ip=${RELAY_PUBLIC_IP}，控制面 udp/9443，数据面 udp/${DATA_START}-${DATA_END}，systemd unit localtrans-relay）；双端均从家庭 LAN 出发经 NAT 到公网中继——真跨网路径`,
      '隔离：双端监听/广播端口基址错开（restartIsolated），本地发现机制上不可达，唯一会话路径=公网中继；清理恢复原端口',
      '服务器侧 Punch/KNOCK journalctl 证据经运维 SSH 通道事后归档进报告目录（场景内不携带服务器凭据）',
      'PSK 经 LT_RELAY_PSK 环境变量一次性注入，不落 git（targets.local.yaml 同级机密纪律）',
      '发送端卡片字节按 e2e-harness §8 已知bug（done×2）容差处理：恰为文件大小整数倍时记档不失败；接收端字节严格核对（交付权威）',
      '截图证据可能降级（huss_laptop 无显示器会话 screenshot 500），失败记入 journal 不判 FAIL',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
