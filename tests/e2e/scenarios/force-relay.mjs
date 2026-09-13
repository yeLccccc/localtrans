// 场景：force-relay（M3c T3 强制走中继 + T2 通道面板 e2e 断言——计划卡
// docs/superpowers/plans/2026-09-08-m3c-smart-routing-ui.md）
//
// ★★ 双机条件跳过场景：任一 PC bridge 未就绪 → SKIP 退出（exit 0 + 报告
//    outcome=skip），已注册 run-all（注册表照常通过）。前置：node lib/deploy.mjs。★★
//
// 验收路径：
//   0) 双 PC 不可达 → SKIP（条件跳过设计）。
//   1) 环境握手 + L3 夹具（keepIdentity+seedTrust 互信，设备名还原）——
//      ⋮ 菜单仅在已信任卡片上出现，播种信任是前置。
//   2) 【拦截门】UI 开启强制走中继（⋮ 菜单勾选，set_force_relay 不进 invoke
//      白名单——set_* 写配置排除原则，e2e 走真实用户点击）→ 中继未配置时
//      invoke connect 必报「中继未配置」（开关在命令层拦截决策、跳过评分的
//      进程级证据）→ 断言 list_devices 条目 force_relay=true。
//   3) 【基线直连】UI 关闭开关 → connect 建会话 → list_channels 当前通道
//      via_relay=false（直连恢复锚）→ 通道面板 UI 断言：dump 对照通道表
//      （T2 数据一致性），行 testid channel-row-{addr} 与「✓ 当前」。
//   4) UI 再开启开关（持久化落 config.json）。
//   5) 隔离重启（lib/relay.mjs restartIsolated：双端端口基址错开，直连不可达，
//      行为级隔离非防火墙）→ 开关仍在（持久化断言：重启后 list_devices
//      force_relay=true）。
//   6) 起中继（huss_laptop）→ 双端设置页 UI 保存中继配置（relay-path 同款
//      enableRelayViaUI）→ 名册可见。
//   7) connect 建会话 → 双端 sessions trusted + 设备视图 viaRelay=true +
//      list_channels 当前通道 via_relay=true + relay debug 日志 Punch/KNOCK
//      ——强制走中继生效的端到端证据。
//   8) UI 关闭开关 → 关中继 → 恢复默认端口重启 → connect → list_channels
//      via_relay=false（关→恢复直连）。
//   9) 收尾：截图/证据/test/end → finally 清理（UI 关 relay → L2 回默认 →
//      L1 → 停 relay 删任务）。
//
// 已知边界（如实声明）：自动重连（FR5）走 connect_pinned 直连路径、不经过
// connect 命令层——强制走中继拦的是用户/编排器发起的 connect 决策（与计划卡
// "开关在命令层拦截决策"一致）；隔离段直连不可达，自动重连静默失败不干扰断言。
//
// 用法：node scenarios/force-relay.mjs（前置：node lib/deploy.mjs 已部署双端）
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { stopPcB } from '../lib/deploy.mjs';
import {
  RELAY, PORT_BASE, relayPublicIp, startRelay, stopRelay, relayLog,
  restartIsolated, restoreAppPorts, removeLaptopTasks,
} from '../lib/relay.mjs';

const SCENARIO = 'force-relay';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runId);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);
const B = new Target('huss_laptop', targets.huss_laptop, journal);
const both = [A, B];
const cfg = targets;

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let pcFp = null;
let laptopFp = null;
let savedPorts = null;
let relayServerForPc = null;
let relayLogText = '';

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

/** 可达探测（短超时）：任一 PC 休眠时 fetch 快速失败/超时 → 调用方走 SKIP 出口 */
async function reachable(t, timeoutMs = 8000) {
  try {
    const v = await t.ok('/api/version', { timeoutMs });
    return v?.bridgeReady === true;
  } catch { return false; }
}

/** 轮询 invoke 白名单命令直至 pred 命中 */
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

/**
 * 设备页 UI 切换「强制走中继」勾选项（⋮ 菜单 → checkbox）。
 * 开关不进 invoke 白名单（set_* 写配置排除原则），走真实用户点击。
 * 落盘以 list_devices 的 force_relay 字段钉死。
 */
async function setForceRelayViaUI(t, peerFp, on) {
  await t.uiNavigate('/transfers'); // 离开当前路由,保证下一步重挂载
  await t.uiNavigate('/devices');
  // DeviceCard 的 ⋮ 菜单按钮 testid 是静态 device-menu-btn,位于 card-{fp} 作用域内
  const cardScope = `[testid=device-card-${peerFp}]`;
  await t.uiWait(cardScope, 15_000);
  const menuSel = `${cardScope} [testid=device-menu-btn]`;
  await t.uiWait(menuSel, 15_000);
  await t.uiClick(menuSel);
  const toggleSel = '[testid=device-force-relay-toggle]';
  await t.uiWait(toggleSel, 8000);
  await t.uiToggle(toggleSel);
  // 落盘钉死(乐观更新即时生效;防重复点击)
  const want = await pollInvoke(t, 'list_devices',
    (devices) => devices.some((d) => d.fingerprint === peerFp && d.force_relay === on),
    { timeoutMs: 25_000, what: `force_relay=${on}` });
  return want;
}

/** 设置页 UI 保存中继配置（relay-path 同款,三防坑:重挂载/回填等待/重存） */
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
    let loaded = false;
    for (let i = 0; i < 30 && !loaded; i++) {
      const tree = await t.uiTree();
      const nameEl = tree.find((e) => e.testId === 'settings-device-name-input');
      loaded = !!nameEl && nameEl.value === cfgBefore.device_name && String(nameEl.value).length > 0;
      if (!loaded) await sleep(300);
    }
    if (!loaded) throw new Error(`${t.name} 设置页配置回填超时`);
    await t.uiInput(SERVER_IN, server, { events: ['blur'] });
    await t.uiInput(PSK_IN, psk, { events: ['blur'] });
    if (!cfgBefore.relay_enabled) {
      await t.uiClick(TOGGLE);
    }
    await sleep(300);
    await t.uiClick(SAVE_BTN);
    for (let i = 0; i < 10; i++) {
      const c = await t.invoke('get_settings');
      if (c.relay_enabled === true && c.relay_server === server) return `enabled, server=${c.relay_server}`;
      await sleep(500);
    }
    console.log(`  WARN  ${t.name} 中继保存未生效（第 ${attempt} 次），重试`);
  }
  throw new Error(`${t.name} 中继配置三次保存均未生效`);
}

/** 设置页 UI 关闭中继并清空字段（清理路径,尽力而为） */
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
  }
  throw new Error(`${t.name} 中继停用未生效`);
}

/** UI 恢复设备名（L3 后 config.json 被清 → 出厂名;夹具必须还原） */
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

/** 夹具恢复:L3 keepIdentity+seedTrust 互播 + 设备名还原(connect_memory.json 已入 L3 清单) */
async function restorePcFixture() {
  const seed = {
    huss_pc: [{ fingerprint: laptopFp, name: 'huss_laptop' }],
    huss_laptop: [{ fingerprint: pcFp, name: 'huss_pc' }],
  };
  await reset(3, both, { keepIdentity: true, seedTrust: seed });
  await restoreDeviceName(A, 'huss_pc');
  await restoreDeviceName(B, 'huss_laptop');
  return 'L3 重置 + 互播信任 + 设备名还原';
}

/** 清理（finally 必达；各步尽力而为） */
async function cleanup() {
  console.log('\n[force-relay] 清理：UI 关 relay → 恢复端口 → L2 回默认 → L1 → 停 relay');
  for (const t of [A, B]) {
    try { note(`${t.name} 清理: ${await disableRelayViaUI(t)}`); }
    catch (e) { note(`${t.name} 清理关 relay 失败: ${e.message}`); }
  }
  if (savedPorts) {
    try { note(`恢复 config.json 原端口: ${await restoreAppPorts(savedPorts, targets)}`); savedPorts = null; }
    catch (e) { note(`恢复端口失败: ${e.message}`); }
  }
  try { note(await reset(2, both)); } catch (e) { note(`清理 L2 失败: ${e.message}`); }
  try { note(await reset(1, both)); } catch (e) { note(`清理 L1 失败: ${e.message}`); }
  try { note(await stopRelay(targets)); } catch (e) { note(`清理停 relay 失败: ${e.message}`); }
  try { await removeLaptopTasks(targets); note('清理: LT-E2E-PB 任务已删'); }
  catch (e) { note(`清理删任务失败: ${e.message}`); }
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---- 条件跳过出口 ----
  const aUp = await reachable(A);
  const bUp = aUp ? await reachable(B) : false;
  if (!aUp || !bUp) {
    const msg = `SKIP：${!aUp ? 'huss_pc' : 'huss_laptop'} 不可达（bridge 未就绪）——双机条件跳过`;
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

  await step('环境握手：双 PC bridge 就绪（记录指纹）', async () => {
    for (const t of both) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      versions[t.name] = v;
    }
    pcFp = await fingerprintOf(A);
    laptopFp = await fingerprintOf(B);
    if (pcFp?.length !== 64 || laptopFp?.length !== 64) throw new Error(`指纹异常: ${pcFp} / ${laptopFp}`);
    note(`预运行指纹 huss_pc=${pcFp} huss_laptop=${laptopFp}`);
    return `fpA=${pcFp.slice(0, 8)}… fpB=${laptopFp.slice(0, 8)}…`;
  });

  await step('夹具：L3 keepIdentity+seedTrust 互播 + 设备名还原 + begin', async () => {
    const seed = {
      huss_pc: [{ fingerprint: laptopFp, name: 'huss_laptop' }],
      huss_laptop: [{ fingerprint: pcFp, name: 'huss_pc' }],
    };
    await reset(3, both, { keepIdentity: true, seedTrust: seed });
    await restoreDeviceName(A, 'huss_pc');
    await restoreDeviceName(B, 'huss_laptop');
    await A.beginTest(SCENARIO);
    await B.beginTest(SCENARIO);
    return '互信播种完成（⋮ 菜单前置:已信任卡片）';
  });

  await step('【拦截门】UI 开启强制走中继 → 中继未配置时 connect 报「中继未配置」', async () => {
    await setForceRelayViaUI(A, laptopFp, true);
    // 命令层拦截决策的进程级证据:无会话+无中继,connect 必须报「中继未配置」
    // (若拦截失效,会落回本地发现直连路径——此刻直连可达,connect 将成功)
    let errText = '';
    try {
      await A.invoke('connect', { fingerprint: laptopFp });
    } catch (e) {
      // ApiError.detail.error 才是命令错误本体;e.message 是 invoke 包装文案
      errText = String(e.detail?.error ?? e.message ?? e);
    }
    if (!errText.includes('中继未配置')) {
      throw new Error(`强制走中继拦截失效: connect 应报「中继未配置」,实际: ${errText || '成功(直连建会话?)'}`);
    }
    const d = await A.invoke('list_devices');
    const entry = d.find((x) => x.fingerprint === laptopFp);
    if (!entry?.force_relay) throw new Error(`list_devices 未注记 force_relay: ${JSON.stringify(entry)}`);
    return `connect 拦截生效(${errText.trim().slice(0, 40)});force_relay=true 已注记`;
  });

  await step('【基线直连】UI 关开关 → connect → 通道表当前通道 via_relay=false', async () => {
    await setForceRelayViaUI(A, laptopFp, false);
    await A.invoke('connect', { fingerprint: laptopFp });
    await A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
      { timeoutMs: 30_000, what: '直连会话 trusted' });
    await pollInvoke(A, 'list_channels',
      (rows) => rows.some((r) => r.fingerprint === laptopFp && r.current && r.via_relay === false),
      { timeoutMs: 30_000, what: '当前通道 via_relay=false(直连)' });
    return '直连会话建立,当前通道 via_relay=false(恢复锚)';
  });

  await step('【T2 通道面板】UI 点通道标签 → 面板行与通道表数据一致', async () => {
    await A.uiNavigate('/transfers');
    await A.uiNavigate('/devices');
    await A.uiClick('[testid=device-channel-label]');
    await A.uiWait('[testid=device-channel-panel]', 8000);
    await A.uiWait('[testid^="channel-row-"]', 8000);
    await A.uiWait('[testid=channel-reprobe-btn]', 5000);
    // 数据一致性:面板行地址集 == 通道表中该设备记录地址集
    const tree = await A.uiTree();
    const panelRows = tree.filter((e) => (e.testId || '').startsWith('channel-row-'))
      .map((e) => e.testId.slice('channel-row-'.length));
    const tableRows = (await A.invoke('list_channels'))
      .filter((r) => r.fingerprint === laptopFp).map((r) => r.addr);
    if (panelRows.length !== tableRows.length) {
      throw new Error(`面板行数(${panelRows.length}) != 通道表记录数(${tableRows.length})`);
    }
    for (const addr of tableRows) {
      if (!panelRows.includes(addr)) throw new Error(`面板缺行: ${addr}(面板=${JSON.stringify(panelRows)})`);
    }
    // 重测按钮可用(有 current 记录)+ 点击触发 probe_now_peer 成功
    await A.uiClick('[testid=channel-reprobe-btn]');
    await sleep(1500);
    await A.uiClick('[testid=channel-panel-close-btn]');
    const shot = await A.screenshot(join(runDir, 'v-channel-panel.png'), { soft: true });
    return { detail: `面板 ${panelRows.length} 行与通道表一致(${panelRows.join(', ')});重测已触发`, artifacts: shot?.skipped ? [] : [F('v-channel-panel.png')] };
  });

  await step('【开启+持久化】UI 再开启开关 → 截图角标 → 隔离重启后仍开启', async () => {
    await setForceRelayViaUI(A, laptopFp, true);
    // 角标可见(卡上 force-relay badge)
    await A.uiNavigate('/transfers');
    await A.uiNavigate('/devices');
    const badgeSel = '[testid=device-force-relay-badge]';
    try { await A.uiWait(badgeSel, 8000); } catch {
      throw new Error(`强制中继角标未出现: ${badgeSel}(force_relay 已落盘但 UI 未渲染)`);
    }
    const shot = await A.screenshot(join(runDir, 'v-force-relay-badge.png'), { soft: true });
    // 隔离重启:双端端口基址错开(直连不可达);config.json 持久化 → 开关应保留
    const { saved } = await restartIsolated(targets);
    savedPorts = saved;
    await A.beginTest(SCENARIO);
    await B.beginTest(SCENARIO);
    const d = await A.invoke('list_devices');
    const entry = d.find((x) => x.fingerprint === laptopFp);
    if (!entry?.force_relay) throw new Error(`隔离重启后开关丢失: ${JSON.stringify(entry)}`);
    return { detail: '角标可见;重启后 force_relay=true 保持(持久化生效)', artifacts: shot?.skipped ? [] : [F('v-force-relay-badge.png')] };
  });

  await step('起中继 + 双端启用:relay connected + 名册可见', async () => {
    relayServerForPc = relayPublicIp(targets);
    const { logBrief } = await startRelay(targets);
    relayLogText = logBrief;
    const r1 = await enableRelayViaUI(A, `${relayServerForPc}:${RELAY.controlPort}`, RELAY.psk);
    const r2 = await enableRelayViaUI(B, `127.0.0.1:${RELAY.controlPort}`, RELAY.psk);
    await Promise.all([
      pollInvoke(A, 'relay_status', (r) => r.connected === true && r.devices >= 1,
        { timeoutMs: 30_000, what: 'A relay connected + 名册含对端' }),
      pollInvoke(B, 'relay_status', (r) => r.connected === true && r.devices >= 1,
        { timeoutMs: 30_000, what: 'B relay connected + 名册含对端' }),
    ]);
    return `relay@huss_laptop(${relayServerForPc}:${RELAY.controlPort});A ${r1};B ${r2}`;
  });

  await step('【强制生效】connect → 会话经中继:viaRelay=true + 通道表 via_relay + Punch 日志', async () => {
    await A.invoke('connect', { fingerprint: laptopFp });
    await Promise.all([
      A.pollUntil((s) => s.sessions.find((x) => x.peer === laptopFp && x.trusted),
        { timeoutMs: 30_000, what: 'A sessions trusted' }),
      B.pollUntil((s) => s.sessions.find((x) => x.peer === pcFp && x.trusted),
        { timeoutMs: 30_000, what: 'B sessions trusted' }),
    ]);
    // 设备视图:路径是中继(隔离下本地发现不可达,不可能翻直连)
    const sA = await A.state();
    if (!peerEntry(sA, laptopFp)?.viaRelay) throw new Error(`A 视角 viaRelay=false: ${JSON.stringify(peerEntry(sA, laptopFp))}`);
    // 通道表:当前通道 via_relay=true
    await pollInvoke(A, 'list_channels',
      (rows) => rows.some((r) => r.fingerprint === laptopFp && r.current && r.via_relay === true),
      { timeoutMs: 30_000, what: '当前通道 via_relay=true' });
    // relay 进程 debug 日志:Punch/KNOCK 行(走了中继的进程级证据)
    const log = await relayLog(targets);
    const punch = log.split('\n').filter((l) => l.includes('会话端口分配成功') || l.includes('KNOCK'));
    if (!punch.length) throw new Error('relay 日志无 Punch/KNOCK 行(路径未走中继?)');
    relayLogText = log;
    const shot = await A.screenshot(join(runDir, 'v-forced-relay-session.png'), { soft: true });
    return { detail: '会话 trusted + viaRelay=true + 通道表 via_relay=true + relay Punch/KNOCK 证据', artifacts: shot?.skipped ? [] : [F('v-forced-relay-session.png')] };
  });

  await step('【关闭恢复】UI 关开关 → 关中继 → 恢复默认端口重启 → connect 直连', async () => {
    await setForceRelayViaUI(A, laptopFp, false);
    for (const t of both) {
      try { note(`${t.name} 关 relay: ${await disableRelayViaUI(t)}`); }
      catch (e) { note(`${t.name} 关 relay 失败(继续): ${e.message}`); }
    }
    await restoreAppPorts(savedPorts, targets);
    savedPorts = null;
    // 双端回默认端口重启(reset(2)=杀进程+默认环境拉起)
    await reset(2, both);
    await A.beginTest(SCENARIO);
    await B.beginTest(SCENARIO);
    await A.invoke('connect', { fingerprint: laptopFp });
    await A.pollUntil((s) => s.sessions.some((x) => x.peer === laptopFp && x.trusted),
      { timeoutMs: 60_000, what: '直连会话恢复 trusted' });
    await pollInvoke(A, 'list_channels',
      (rows) => rows.some((r) => r.fingerprint === laptopFp && r.current && r.via_relay === false),
      { timeoutMs: 30_000, what: '当前通道 via_relay=false(直连恢复)' });
    const shot = await A.screenshot(join(runDir, 'v-restored-direct.png'), { soft: true });
    return { detail: '开关关闭持久化;直连会话恢复,通道表 via_relay=false', artifacts: shot?.skipped ? [] : [F('v-restored-direct.png')] };
  });

  await step('收尾：证据 + test/end', async () => {
    const arts = [];
    const ev = await collectEvidence(both, runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    try { await A.endTest('pass'); } catch (e) { note(`A test/end: ${e.message}`); }
    try { await B.endTest('pass'); } catch (e) { note(`B test/end: ${e.message}`); }
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
}

// finally 必达清理（成功/失败都跑）
try { await cleanup(); } catch (e) { note(`清理异常: ${e.message}`); }

const outcome = failureMsg ? 'fail' : (steps.length ? 'pass' : 'skip');
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      'M3c T3 强制走中继:开关 per 设备持久化(core store::Config.force_relay_map → config.json);开启时 connect 命令层拦截决策跳过评分直选中继(评分/切换 core 语义不动)',
      '开关切换经设备页 ⋮ 菜单真实 UI 点击(set_force_relay 不进 invoke 白名单——set_* 写配置排除原则);落盘以 list_devices.force_relay 钉死',
      '拦截门证据:无中继+无会话时 connect 报「中继未配置」(拦截失效则会落回直连并成功)',
      '隔离经双端端口基址错开(restartIsolated,行为级隔离);中继拓扑/启用流程与 relay-path 同款(lib/relay.mjs 复用)',
      '已知边界:自动重连(FR5)走 connect_pinned 直连路径不经 connect 命令——强制走中继拦用户/编排器发起的 connect(计划卡口径);隔离段直连不可达,自动重连静默失败不干扰断言',
      '附带 T2 通道面板 e2e 断言:面板行(channel-row-{addr})与 list_channels 数据一致 + 重测按钮(probe_now_peer)',
      '清理:UI 关 relay → 恢复 config.json 原端口 → L2 回默认 → L1 → 停 relay 删任务',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'fail' ? 1 : 0);
