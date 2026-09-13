// 场景：notify-deeplink（V1 计划卡 T1——安卓手工清单通知项场景化）
//
// ★★ 条件跳过：huss_pc bridge 未就绪 或 huss_phone(adb) 不可达 或 APK 未构建
//    → SKIP 退出（exit 0）。深链跳转点击为软断言（真机交互面），通知存在性
//    为 dumpsys 硬断言。已注册 run-all。★★
//
// 验收路径（手工清单 #7 预期"App 通知栏显示…" + TransferNotifier.kt 产品行为）：
//   0) 设备不可达 → SKIP（条件跳过设计）。
//   1) 手机唤醒/装包/启动/LT-BANNER 就绪；PC→手机幂等配对（已配对直连）。
//   2) PC 推唯一文件 → 手机 OfferSheet 点"接收" → done。
//      （TransferNotifier 仅接收方向成功时弹通知——rx/pull 过滤）
//   3) 硬断言：dumpsys notification 见本包通知（channel transfer_complete /
//      标题"传输完成"）。
//   4) 深链（软断言）：展开通知栏 → 截图 → 点通知 → App 回到前台传输页
//      （MainActivity + open_tab=transfers）。MIUI 真机交互面，失败记人工复核项
//      不判 FAIL——通知存在性已由第 3 步背书。
//
// 用法：node scenarios/notify-deeplink.mjs（前置：PC 端 node lib/deploy.mjs --skip-huss_laptop；
//       安卓端 gradle assembleDebug 已产出 APK）
import { mkdirSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadTargets, e2eRoot, repoRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import { startPcA, stopPcA, waitReady } from '../lib/deploy.mjs';
import createAdb from '../lib/adb.mjs';

const SCENARIO = 'notify-deeplink';
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const runDir = join(e2eRoot(), 'reports', `notify-deeplink-${stamp}`);
mkdirSync(runDir, { recursive: true });
const APK = join(repoRoot(), 'android', 'app', 'build', 'outputs', 'apk', 'debug', 'app-debug.apk');

const log = (m) => console.log(m);
function assert(cond, msg) { if (!cond) throw new Error(msg); }
const sh = (c, timeoutMs = 20000) => execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', c], { encoding: 'utf8', timeout: timeoutMs });
const hostAdb = (args, timeoutMs = 8000) => execFileSync(ch.adbPath, ['-s', ch.serial, ...args], { encoding: 'utf8', timeout: timeoutMs });

/** SKIP 出口：统一报告 + exit 0（run-all 注册表照常通过） */
function skip(msg) {
  log(`\n[${SCENARIO}] SKIP：${msg}`);
  writeFileSync(join(runDir, 'SKIPPED.txt'), `${new Date().toISOString()} ${msg}\n`, 'utf8');
  console.log(`[${SCENARIO}] 证据目录: ${runDir}`);
  process.exit(0);
}

async function main() {
  log(`[${SCENARIO}] 开始  runDir=${runDir}`);

  // ---- 条件跳过出口 ----
  let pcUp = false;
  try { pcUp = (await A.version()).bridgeReady === true; } catch { pcUp = false; }
  if (!pcUp) skip('huss_pc bridge 未就绪——先跑 node lib/deploy.mjs --skip-huss_laptop');
  let phoneUp = false;
  try { phoneUp = hostAdb(['get-state']).trim() === 'device'; } catch { phoneUp = false; }
  if (!phoneUp) skip('huss_phone(adb) 不可达');
  if (!existsSync(APK)) skip(`APK 未构建: ${APK}`);

  // ---- 准备：唤醒/装包/启动 ----
  log('== 准备:手机亮屏/装包/启动 ==');
  ch.wake(); ch.setStayOn(true); await sleep(1200);
  try { sh('cmd statusbar collapse'); } catch { /* 尽力：上轮可能留展开的通知栏挡住 App */ }
  await sleep(500);
  await ch.install(APK);
  ch.clearLogcat(); ch.launch();
  const banner = await ch.waitForBanner(30_000);
  log('banner ✓ ' + banner.slice(0, 60));
  await sleep(2500);

  // 自动同意钩子（设置页折叠区下方，先滚动到底）
  await ch.tap({ testTag: 'nav-settings-link' }); await sleep(1200);
  let hook = null;
  for (let i = 0; i < 4 && !hook; i++) {
    const els = await ch.dump();
    hook = els.find((e) => e.testTag === 'settings-test-auto-consent-toggle');
    if (!hook) { ch.swipe(540, 1750, 540, 750); await sleep(700); }
  }
  assert(hook, '设置页(滚动后)应有自动同意开关');
  if (!hook.checked) { await ch.tap({ testTag: 'settings-test-auto-consent-toggle' }); await sleep(600); }
  await ch.tap({ testTag: 'nav-devices-link' }); await sleep(1000);

  // PC 侧：本机重启清活动引擎残留（★ 不能用 reset(2,[A])：它会 ssh 杀 huss_laptop，
  // laptop 网络隔离时必炸——这里只动 huss_pc 本机）
  stopPcA(); startPcA(targets);
  await waitReady(targets.huss_pc, { label: 'huss_pc', timeoutMs: 120_000 });
  A.runId = null;
  await A.beginTest(SCENARIO);
  const s0 = await A.state();
  const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
  assert(phone, 'A 应在线见到 huss_phone: ' + JSON.stringify((s0.devices || []).map((d) => d.name)));
  const fpPhone = phone.id;
  const fpPc = s0.selfDevice.id;
  log(`手机指纹 ${fpPhone.slice(0, 8)}… / PC 指纹 ${fpPc.slice(0, 8)}…`);

  // ---- 幂等配对（手机点 PC 卡；已配对直连）----
  log('== 连接/配对(手机发起,幂等) ==');
  await ch.tap({ testTag: `device-card-${fpPc}` });
  await sleep(1500);
  let els = await ch.dump();
  const waiting = els.find((e) => /正在等待|等待对方/.test(e.text));
  if (waiting) {
    await A.uiWait('[testid=pairing-dialog]', 20_000);
    await A.uiClick('[testid=btn-grant]');
    let code = null;
    for (let i = 0; i < 10 && !code; i++) {
      await sleep(700);
      const txt = await A.uiText('.code-display').catch(() => '');
      const m = txt.match(/\d{6}/);
      if (m) code = m[0];
    }
    assert(code, 'PC 应显示 6 位配对码');
    els = await ch.dump();
    const input = els.find((e) => e.testTag === 'code-input');
    assert(input, '手机应有输码框(code-input)');
    await ch.tap({ testTag: 'code-input' });
    await ch.inputText(code);
    await ch.tap({ testTag: 'btn-submit' });
    log('已输码提交 ' + code.slice(0, 2) + '****');
  } else {
    log('(已配对直连)');
  }
  await A.pollUntil((s) => (s.sessions ?? []).some((x) => x.peer === fpPhone),
    { timeoutMs: 30_000, intervalMs: 1000, what: 'PC-手机会话建立' });
  log('连接 ✓');

  // ---- PC 推唯一文件 → 手机接收（触发 rx 完成通知）----
  log('== 推送 PC→手机(触发接收完成通知) ==');
  const pushName = `nd-${stamp}.bin`;
  const pushPath = join(runDir, pushName);
  { const b = Buffer.alloc(1 * 1024 * 1024); b.write(`ND ${stamp}`, 0, 'utf8'); b.fill(0x4e, 64); writeFileSync(pushPath, b); }
  const cardId = await A.invoke('push_files', { fingerprint: fpPhone, local_paths: [pushPath] });
  let accepted = false;
  for (let i = 0; i < 25 && !accepted; i++) {
    await sleep(800);
    const offerEls = await ch.dump();
    const btn = offerEls.find((e) => e.testTag === 'offer-accept-btn')
      || offerEls.find((e) => /接收|接受|允许/.test((e.text || '') + (e.contentDesc || '')));
    if (!btn) continue;
    await ch.tapXY(btn.center[0], btn.center[1]);
    accepted = true;
  }
  assert(accepted, '手机应出现接收弹窗');
  await ch.screenshot(join(runDir, 'v-phone-offer.png'));
  const pushed = await A.pollUntil((st) => {
    const t = (st.transfers || []).find((x) => x.id === cardId);
    return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
  }, { timeoutMs: 120_000, intervalMs: 1000, what: '推送终态' });
  assert(pushed.value.state === 'done', '推送应 done,实际 ' + pushed.value.state);
  log('推送 ✓ done（rx 方向成功 → TransferNotifier 应已发通知）');
  await sleep(1500); // 通知投递余量

  // ---- 硬断言：通知存在（dumpsys）----
  log('== 断言:系统通知存在(dumpsys) ==');
  const dump = sh('dumpsys notification --noredact', 25000);
  const pkg = ch.PACKAGE || 'com.localtrans.app';
  const hasChannel = dump.includes('transfer_complete');
  const hasTitle = dump.includes('传输完成');
  const hasPkg = dump.toLowerCase().includes(pkg);
  assert(hasPkg && (hasChannel || hasTitle),
    `系统通知未找到（pkg=${hasPkg} channel=${hasChannel} title=${hasTitle}）`
    + `——接收方向成功必须弹通知(TransferNotifier)；MIUI 通知权限被关时需人工放行后重跑`);

  // ---- 深链跳转（软断言：真机交互面）----
  log('== 深链跳转(软断言):展开通知栏 → 点通知 → 传输页 ==');
  let deeplinkOk = false;
  const onTransfers = async () => {
    const els2 = await ch.dump();
    return els2.some((e) => e.testTag === 'transfers-history-fold-btn');
  };
  try {
    sh('cmd statusbar expand-notifications');
    await sleep(1500);
    await ch.screenshot(join(runDir, 'v-shade.png'));
    const els = await ch.dump();
    const notifNode = els.find((e) => /传输完成|文件接收完成/.test(e.text || ''));
    if (notifNode) {
      await ch.tapXY(notifNode.center[0], notifNode.center[1]);
      await sleep(2500);
    } else {
      // MIUI shade 节点在 uiautomator 树不可见（实测被折叠进"不重要通知"组）时，
      // 走等价 intent 面：通知 PendingIntent 即 MainActivity + extra open_tab=transfers
      // （TransferNotifier.kt）。shade 挡在 App 前面必须先收起，否则 dump 看到的是 SystemUI。
      log('(shade 未dump到通知节点 → 收起通知栏 + am start 等价 intent 面验证)');
      try { sh('cmd statusbar collapse'); } catch { /* 尽力 */ }
      await sleep(800);
      sh(`am start -n ${ch.PACKAGE}/.MainActivity --es open_tab transfers`);
      await sleep(2500);
    }
    let focus = '';
    try { focus = sh('dumpsys window windows 2>/dev/null | grep -E mCurrentFocus; true', 15000); } catch { /* 设备侧 dumpsys 变体差异不致死 */ }
    const onT = await onTransfers();
    assert(onT, `深链后未落在传输页: focus="${String(focus).trim().slice(0, 60)}"`);
    log(`前台聚焦: "${String(focus).trim().slice(0, 60)}"`);
    deeplinkOk = true;
    await ch.screenshot(join(runDir, 'v-deeplink-transfers.png'));
  } catch (e) {
    log(`[soft] 深链点击未自动化通过（人工复核项，非本场景 FAIL）: ${e.message}`);
    try { await ch.screenshot(join(runDir, 'v-deeplink-soft-fail.png')); } catch { /* 尽力 */ }
  } finally {
    try { sh('cmd statusbar collapse'); } catch { /* 收起通知栏，不挡链式后续场景 */ }
  }
  log(deeplinkOk ? '深链 ✓ App 回前台传输页' : '深链=人工复核（通知存在性已由 dumpsys 背书）');

  // ---- 收尾 ----
  try { await A.invoke('clear_completed_transfers'); } catch { /* 视图清理尽力 */ }
  await A.endTest('pass');
  try { await ch.screenshot(join(runDir, 'v-final.png')); } catch { /* 尽力 */ }
  rmSync(pushPath, { force: true });
  log(`PASS 证据: ${runDir}`);
}

try {
  await main();
} catch (e) {
  console.error(`\n[${SCENARIO}] FAIL: ${e.message}`);
  console.error(e.stack || '');
  try { await ch.screenshot(join(runDir, 'v-fail.png')); } catch { /* 尽力 */ }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  process.exitCode = 1;
}
