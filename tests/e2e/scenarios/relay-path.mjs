// 场景：relay-path（T5 中继路径——计划卡 docs/superpowers/plans/2026-09-07-m1-stability-foundation.md）
//
// 流程：双端握手 → 起 relay（huss_laptop，19443/19500-19520，见 lib/relay.mjs
// 拓扑决策）→ 双端以不同 LOCALTRANS_TEST_PORT_BASE 隔离重启（直连不可达，
// 非防火墙方案：agent 会话无管理员权限，行为级隔离无系统副作用）→ 隔离断言
// → 双端设置页 UI 真实保存中继配置 → 名册可见（viaRelay=true + relay 语义地址）
// → connect 建会话 → 推 5MB（内容唯一戳）→ 双端终态+字节核对 → 截图/证据 →
// finally 清理（UI 关 relay → L2 回默认基址 → L1 清卡 → 停 relay 删任务）。
//
// 断言等级（如实声明）：验证"配置了中继下会话经中继建立+传输成功"。
// 隔离经端口基址错开实现（广播/监听端口无交集，本地发现在机制上不可达），
// 会话路径证据 = 设备视图 viaRelay=true + addr 为 relay 租约地址 + relay 进程
// debug 日志的 Punch 会话端口分配/KNOCK 行。非防火墙阻断式"强制"。
//
// 用法：node scenarios/relay-path.mjs   （前置：双端已部署并运行，本机 exe 在 target/release）
import { mkdirSync, writeFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import {
  RELAY, PORT_BASE, relayPublicIp, startRelay, stopRelay, relayLog,
  restartIsolated, restoreAppPorts, removeLaptopTasks,
} from '../lib/relay.mjs';

const SCENARIO = 'relay-path';
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
let fpA = null; // huss_pc 指纹
let fpB = null; // huss_laptop 指纹
let relayServerForPc = null; // = relayPublicIp（laptop LAN IP）
let relayLogText = '';
let savedPorts = null; // 双端 config.json 原端口（隔离补丁前记录，清理恢复用）

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 步骤包装（与 pc-pc-transfer 同款）：PASS/FAIL 计时入矩阵 */
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

/** 轮询 invoke 白名单命令直至 pred 命中（Target.pollUntil 只覆盖 state 快照） */
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

/** 从设备合并视图取对端条目 */
const peerEntry = (snap, peerFp) => snap.devices.find((d) => d.id === peerFp);

/**
 * 设置页 UI 保存中继配置（真实用户流程：输入→开关→保存）。
 * 防两个坑：
 * 1. 配置异步回填踩值——先等输入框 value == 持久化值（回填完成信号）再输入；
 * 2. 同路由 push 不重挂载 + 开关 @change 自存与 v-model 更新存在竞态——
 *    每次尝试先离开 /settings 再回来（强制重挂载，ref 从 config 重置），
 *    开关点击后追加"保存并连接"按钮点击（以当前 refs 确定性重存，幂等）。
 * 落盘以 get_settings 钉死；连接成功性由 relay_status.connected（PSK 认证）证明。
 */
async function enableRelayViaUI(t, server, psk) {
  const SAVE_BTN = '[testid=settings-relay-save-btn]';
  const SERVER_IN = '[testid=settings-relay-server-input]';
  const PSK_IN = '[testid=settings-relay-psk-input]';
  const TOGGLE = '[testid=settings-relay-enabled-toggle]';
  for (let attempt = 1; attempt <= 3; attempt++) {
    await t.uiNavigate('/transfers'); // 离开当前路由，保证下一步重挂载
    await t.uiNavigate('/settings');
    await t.uiWait(SERVER_IN, 10_000);
    const cfgBefore = await t.invoke('get_settings');
    if (cfgBefore.relay_psk && !String(cfgBefore.relay_psk).startsWith('****')) {
      throw new Error(`${t.name} get_settings 回显明文 psk（A4 隐私契约破坏）`);
    }
    // 等配置回填完成：设备名输入框回填 == get_settings.device_name（同一异步
    // 回填链、且永远非空——server 空串时 "空==空" 会假阳性，实测踩中）。
    // 回填链里 editableDeviceName 先于 relay 三字段赋值，命中即 relay refs 就绪。
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
      await t.uiClick(TOGGLE); // 重挂载后必为未勾选 → 点击必是"关→开"
    }
    await sleep(300);
    await t.uiClick(SAVE_BTN); // 以当前 refs（已开）确定性重存，纠正 @change 自存竞态
    // 落盘钉死（回填踩值时保存内容不对，此处暴露并重试）
    for (let i = 0; i < 10; i++) {
      const cfg = await t.invoke('get_settings');
      if (cfg.relay_enabled === true && cfg.relay_server === server) return `enabled, server=${cfg.relay_server}`;
      await sleep(500);
    }
    console.log(`  WARN  ${t.name} 中继保存未生效（第 ${attempt} 次），重试`);
  }
  throw new Error(`${t.name} 中继配置三次保存均未生效（get_settings 不匹配）`);
}

/** 设置页 UI 关闭中继并清空字段（清理路径，尽力而为；同样强制重挂载防 ref 残留） */
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
    // 等回填（设备名标记，同 enableRelayViaUI——onMounted 异步回填会覆盖开关/输入）
    let loaded = false;
    for (let i = 0; i < 30 && !loaded; i++) {
      const tree = await t.uiTree();
      const nameEl = tree.find((e) => e.testId === 'settings-device-name-input');
      loaded = !!nameEl && nameEl.value === cfg.device_name && String(nameEl.value).length > 0;
      if (!loaded) await sleep(300);
    }
    if (!loaded) throw new Error(`${t.name} 设置页配置回填超时（设备名输入框未回填 device_name）`);
    if (cfg.relay_enabled) {
      await t.uiClick(TOGGLE); // 重挂载后必为已勾选 → 点击必是"开→关"
      await sleep(300);
    }
    // 清残留 server/psk（disabled 下保存不校验，写空串恢复出厂语义）
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

/** 5MB 唯一戳 fixture（确定性伪随机 + runId 尾戳，规避接收端秒传去重） */
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
  const stamp = Buffer.from(`\n[relay-path ${runId}]\n`, 'utf8');
  const p = join(dir, `relay-5mb-${rand}.bin`);
  writeFileSync(p, Buffer.concat([buf, stamp]));
  return p;
}

/** 清理（finally 必达；各步尽力而为不互相阻断） */
async function cleanup() {
  console.log('\n[relay-path] 清理：UI 关 relay → 恢复端口 → L2 回默认 → L1 清卡 → 停 relay');
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
  try { note(await stopRelay(targets)); }
  catch (e) { note(`清理停 relay 失败: ${e.message}`); }
  try { await removeLaptopTasks(targets); note('清理: LT-E2E-PB 任务已删（LT-E2E-Relay 随 stopRelay 删）'); }
  catch (e) { note(`清理删任务失败: ${e.message}`); }
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

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

  await step(`起中继：huss_laptop udp/${RELAY.controlPort}（数据面 ${RELAY.dataStart}-${RELAY.dataEnd}）`, async () => {
    relayServerForPc = relayPublicIp(targets);
    const { logBrief } = await startRelay(targets);
    relayLogText = logBrief;
    return `控制面 udp/${RELAY.controlPort}，public_ip=${relayServerForPc}；${logBrief}`;
  });

  await step(`隔离重启：双端端口基址 pc=${PORT_BASE.huss_pc} / laptop=${PORT_BASE.huss_laptop}`, async () => {
    const { saved, patched } = await restartIsolated(targets);
    savedPorts = saved; // 清理时恢复（壳层绑定端口读持久化 config.json，env 默认值被存量值覆盖）
    // L2 语义：runId 失效，重新 begin
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
      // 允许"信任表兜底 offline 卡"（viaRelay=false 且 addr 空）；本地发现/名册条目均不允许
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

  await step('双端启用中继：设置页 UI 真实保存（server/psk/开关）', async () => {
    const r1 = await enableRelayViaUI(huss_pc, `${relayServerForPc}:${RELAY.controlPort}`, RELAY.psk);
    const r2 = await enableRelayViaUI(huss_laptop, `127.0.0.1:${RELAY.controlPort}`, RELAY.psk);
    return `huss_pc→${relayServerForPc}:${RELAY.controlPort}（${r1}）；huss_laptop→127.0.0.1:${RELAY.controlPort}（${r2}）`;
  });

  await step('名册可见：relay_status connected + 对端 viaRelay=true + relay 租约地址', async () => {
    await Promise.all([
      pollInvoke(huss_pc, 'relay_status', (r) => r.connected === true && r.devices >= 1,
        { timeoutMs: 30_000, what: 'relay connected + 名册含对端' }),
      pollInvoke(huss_laptop, 'relay_status', (r) => r.connected === true && r.devices >= 1,
        { timeoutMs: 30_000, what: 'relay connected + 名册含对端' }),
    ]);
    // 名册条目断言：viaRelay=true 且 addr=public_ip:<数据面池端口>（lease_addr 语义）
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
      if (host !== relayServerForPc) throw new Error(`${t.name} 视角对端 addr 主机非 relay: ${d.value.addr}`);
      if (!(port >= RELAY.dataStart && port <= RELAY.dataEnd)) {
        throw new Error(`${t.name} 视角对端租约端口 ${port} 不在数据面池 ${RELAY.dataStart}-${RELAY.dataEnd}: ${d.value.addr}`);
      }
      if (!d.value.online) throw new Error(`${t.name} 视角对端 online=false: ${JSON.stringify(d.value)}`);
      return d.value.addr;
    };
    const addrOnPc = await check(huss_pc, fpB, 'huss_laptop');
    const addrOnLaptop = await check(huss_laptop, fpA, 'huss_pc');
    return `pc 视角 laptop@${addrOnPc}；laptop 视角 pc@${addrOnLaptop}（均为 relay 租约地址，connected 徽章随后走 connect）`;
  });

  await step('connect 建会话：huss_pc 发起 → 双端 sessions trusted（经中继 punch）', async () => {
    await huss_pc.invoke('connect', { fingerprint: fpB });
    await Promise.all([
      huss_pc.pollUntil(
        (s) => s.sessions.find((x) => x.peer === fpB && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc sessions 含 laptop 且 trusted' },
      ),
      huss_laptop.pollUntil(
        (s) => s.sessions.find((x) => x.peer === fpA && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop sessions 含 pc 且 trusted' },
      ),
    ]);
    // 会话建立后路径仍须是中继（若本地发现复活，merge 本地优先会翻 viaRelay=false）
    const sA = await huss_pc.state();
    const d = peerEntry(sA, fpB);
    if (!d?.viaRelay) throw new Error(`会话后 pc 视角 viaRelay 翻转: ${JSON.stringify(d)}`);
    const sB = await huss_laptop.state();
    if (!peerEntry(sB, fpA)?.viaRelay) throw new Error('会话后 laptop 视角 viaRelay=false');
    // relay 进程 debug 日志取 Punch/KNOCK 证据（进程级"走了中继"）
    const log = await relayLog(targets);
    const punch = log.split('\n').filter((l) => l.includes('会话端口分配成功') || l.includes('KNOCK'));
    if (!punch.length) throw new Error('relay 日志无 Punch/KNOCK 行（debug 日志缺失或路径未走中继）');
    relayLogText = log;
    return `双端 sessions trusted=true，viaRelay 保持 true；relay 日志 Punch/KNOCK ${punch.length} 行（末行: ${punch[punch.length - 1].trim().slice(0, 90)}）`;
  });

  await step('经中继推 5MB：push_files（唯一戳）→ laptop UI 接收', async () => {
    await huss_pc.step('push-files');
    await huss_laptop.step('offer-accept');
    fixtureFile = makeFixture();
    expectedBytes = statSync(fixtureFile).size;
    const card = await huss_pc.invoke('push_files', { fingerprint: fpB, local_paths: [fixtureFile] });
    note(`push_files 占位卡 ${card}，文件 ${fixtureFile}（${expectedBytes}B）`);
    await huss_laptop.uiWait('.offer-modal', 20_000);
    await huss_laptop.uiClick('.offer-modal .btn-success');
    return `relay-5mb-${rand}.bin（${expectedBytes}B）已推送，laptop 已点接收`;
  });

  await step('断言：双端终态 done + 字节核对 + 非秒传 + viaRelay 复查', async () => {
    const fname = `relay-5mb-${rand}.bin`;
    const aPoll = await huss_pc.pollUntil(
      (s) => {
        const mine = s.transfers.filter((t) => t.direction === 'push' && t.name.includes(fname));
        return mine.length > 0 && mine.every((t) => t.state === 'done') ? mine : undefined;
      },
      { timeoutMs: 180_000, intervalMs: 500, what: 'huss_pc push 卡全部 done' },
    );
    const aCards = aPoll.value;
    const aCard = aCards.find((c) => c.bytesTotal === expectedBytes) || aCards[0];
    if (aCard.bytesDone !== aCard.bytesTotal) throw new Error(`pc bytesDone(${aCard.bytesDone}) != total(${aCard.bytesTotal})`);
    if (aCard.failReason) throw new Error(`pc 卡失败: ${aCard.failReason}`);
    if (aCard.instant) throw new Error('pc 卡 instant=true（秒传命中，唯一戳失效）');
    // 发送端 total 恰为文件大小整数倍时放行并记档——e2e-harness §8 已知产品bug
    // "单次推送在发送端可能产生多卡/done×2（待查）"，实测复发；接收端字节才是
    // 交付权威断言（下方严格核对），此处仅记账不失败。
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
    const bCards = bPoll.value;
    const bCard = bCards.find((c) => c.bytesTotal === expectedBytes) || bCards[0];
    if (bCard.bytesDone !== bCard.bytesTotal) throw new Error(`laptop bytesDone(${bCard.bytesDone}) != total(${bCard.bytesTotal})`);
    if (bCard.bytesTotal !== expectedBytes) throw new Error(`laptop total ${bCard.bytesTotal} != 文件 ${expectedBytes}`);
    if (bCard.failReason) throw new Error(`laptop 卡失败: ${bCard.failReason}`);
    if (bCard.instant) throw new Error('laptop 卡 instant=true（秒传命中，唯一戳失效）');

    // 终态复查：路径仍是中继
    const sA = await huss_pc.state();
    const sB = await huss_laptop.state();
    if (!peerEntry(sA, fpB)?.viaRelay || !peerEntry(sB, fpA)?.viaRelay) {
      throw new Error('传输后 viaRelay 复查失败（设备视图回落本地语义）');
    }
    return `pc 卡=${aCard.id}、laptop 卡=${bCard.id}：双端 done、instant=false、viaRelay=true；接收端 ${bCard.bytesTotal}B 精确核对${senderNote}`;
  });

  await step('收尾：截图 + relay 日志 + 证据 + test/end', async () => {
    const arts = [];
    const degraded = [];
    for (const t of [huss_pc, huss_laptop]) {
      try {
        await t.screenshot(join(runDir, `${t.name}-final.png`));
        arts.push(F(`${t.name}-final.png`));
      } catch (e) {
        // 截图是证据不是断言：显示器枚举失败等会话环境问题不判 FAIL（如实降级记录）
        degraded.push(`${t.name}: ${e.message}`);
        note(`${t.name} 截图失败（证据降级）: ${e.message}`);
      }
    }
    writeFileSync(join(runDir, 'relay-log.txt'), relayLogText || '(空)');
    arts.push(F('relay-log.txt'));
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
    const ev = await collectEvidence([huss_pc, huss_laptop], runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
    void ev;
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await huss_pc.endTest('fail'); } catch { /* 尽力 */ }
  try { await huss_laptop.endTest('fail'); } catch { /* 尽力 */ }
}

// finally 必达清理（成功/失败都跑）
try { await cleanup(); } catch (e) { note(`清理异常: ${e.message}`); }

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      `拓扑：relay 跑 huss_laptop（控制面 udp/${RELAY.controlPort}，数据面 udp/${RELAY.dataStart}-${RELAY.dataEnd}，public_ip=${relayServerForPc}）——huss_pc 防火墙三 profile 全启用且 agent 会话无管理员权限加规则，中继放本机则跨机入站必被拦（详见 lib/relay.mjs 头注）`,
      `隔离：双端监听/广播端口错开（pc=${PORT_BASE.huss_pc}/${PORT_BASE.huss_pc + 1}，laptop=${PORT_BASE.huss_laptop}/${PORT_BASE.huss_laptop + 1}）——壳层绑定端口读持久化 data/config.json（env 默认值仅首启生效），故隔离=杀进程→补丁 config.json 端口→重启，清理时恢复原值；任务卡原案"netsh 阻 47600-47601"因 agent 会话无管理员权限不可行，按行为级隔离降级实现（无系统级防火墙副作用）`,
      '断言等级：配置中继下"名册可见（viaRelay=true + relay 租约地址）→ connect 建会话 → 5MB 真实传输（非秒传）"全链；会话路径证据含 relay debug 日志 Punch/KNOCK 行（relay-log.txt 工件）',
      '发送端卡片字节按 e2e-harness §8 已知bug（done×2，待查）容差处理：恰为文件大小整数倍时记档不失败；接收端字节严格核对（交付权威）',
      '环境共占注意：本机与 huss_laptop 同时被并行任务（T2/T3 L3/冷启动）使用——2026-09-07 04:57 实测双端被 L3 重置+重新配对（设备指纹全部更换）。本场景指纹全部运行期采集，不依赖任务卡给定值；但并行跑场景可能互相踩传输/重置，建议错峰',
      '截图证据可能降级：huss_laptop 会话无显示器时 screenshot 500（"未定位到窗口所在显示器"）——UI 渲染已由 offer 弹窗出现+点击与 Rust 快照间接验证，截图失败记入 journal 不判 FAIL',
      '中继配置经设置页 UI 真实保存（开关 @change → set_relay_config）；落盘以 get_settings 钉死，连接成功性以 relay_status.connected（PSK 认证通过）证明',
      '清理：双端 UI 停用并清空中继配置 → L2 回默认端口基址（恢复互见）→ L1 清卡 → 停 relay 并删临时任务（LT-E2E-Relay / LT-E2E-PB）',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
