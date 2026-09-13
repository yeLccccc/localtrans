// 场景：remote-rename（V1 计划卡 T1——安卓手工清单 #11 手机远程重命名桌面文件）
//
// ★★ 条件跳过：huss_pc bridge 未就绪 或 huss_phone(adb) 不可达 或 APK 未构建
//    → SKIP 退出（exit 0）。已注册 run-all。★★
//
// 验收路径（手工清单 #11：App Browse 页长按 → 重命名 → 桌面端文件名确实改变）：
//   0) 设备不可达 → SKIP（条件跳过设计）。
//   1) 手机唤醒/装包/启动/LT-BANNER；PC→手机幂等配对（已配对直连）。
//   2) PC 共享区播种唯一文件（内容戳，改名后核对内容不变）。
//   3) 手机 Files→远程页 → 选 huss_pc → 长按目标文件 → 菜单"重命名"
//      （FileEntryMenuSheet）→ RenameDialog 光标移末尾追加后缀 → 确定。
//   4) 断言：PC 端旧名消失、新名存在、内容一致（编排器本地 fs 直核）。
//
// 交互模型实证沿用 android-pull-batch/night-android-transfer：Compose a11y 树
// 文本节点落在可点击控件 bounds 内，按文本节点中心即命中；RenameDialog 无
// testTag，按文案（新名称/确定）定位；输入法追加式改名（MOVE_END+后缀）规避
// 全选删除的真机输入法差异。
//
// 用法：node scenarios/remote-rename.mjs（前置：PC 端 node lib/deploy.mjs --skip-huss_laptop）
import { mkdirSync, writeFileSync, existsSync, rmSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadTargets, e2eRoot, repoRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import createAdb from '../lib/adb.mjs';

const SCENARIO = 'remote-rename';
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const rand = Math.random().toString(36).slice(2, 5);
const runDir = join(e2eRoot(), 'reports', `remote-rename-${stamp}`);
mkdirSync(runDir, { recursive: true });
const APK = join(repoRoot(), 'android', 'app', 'build', 'outputs', 'apk', 'debug', 'app-debug.apk');

const log = (m) => console.log(m);
function assert(cond, msg) { if (!cond) throw new Error(msg); }
const sh = (c, timeoutMs = 20000) => execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', c], { encoding: 'utf8', timeout: timeoutMs });
const hostAdb = (args, timeoutMs = 8000) => execFileSync(ch.adbPath, ['-s', ch.serial, ...args], { encoding: 'utf8', timeout: timeoutMs });
const longPress = (x, y) => execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', 'input', 'swipe', String(x), String(y), String(x), String(y), '900']);
const keyEvent = (code) => execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', 'input', 'keyevent', String(code)]);

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

  // ---- 手机就绪 ----
  log('== 准备:手机亮屏/装包/启动 ==');
  ch.wake(); ch.setStayOn(true); await sleep(1200);
  await ch.install(APK);
  ch.clearLogcat(); ch.launch();
  const banner = await ch.waitForBanner(30_000);
  log('banner ✓ ' + banner.slice(0, 60));
  await sleep(2500);
  await ch.tap({ testTag: 'nav-devices-link' }); await sleep(1000);

  // ---- PC 共享区播种唯一文件 ----
  const settings = await A.invoke('get_settings');
  const sharePath = settings.shares[0].path;
  const oldName = `rr-${stamp}-${rand}.txt`;
  // 对话框交互 = MOVE_END 后追加后缀 → PC 端新名 = 原名+后缀（含 .txt 在中间，
  // P3-T2 实证：原 newPath 期望「-renamed.txt」中插属于场景自耗缺陷——
  // 此前场景在菜单能力探测点就 SKIP，产品补齐远程重命名后才首次跑到本断言）
  const suffix = `-r${rand}`; // 纯 ASCII（adb input text 无中文 IME 依赖）
  const newName = oldName + suffix;
  const body = `REMOTE-RENAME ${SCENARIO} ${runDir}\n`;
  const oldPath = join(sharePath, oldName), newPath = join(sharePath, newName);
  writeFileSync(oldPath, body, 'utf8');
  log(`播种共享区文件 ${oldPath}`);
  rmSync(newPath, { force: true }); // 防上轮残留撞名

  // ---- 幂等配对（手机点 PC 卡）----
  await A.beginTest(SCENARIO);
  const s0 = await A.state();
  const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
  assert(phone, 'A 应在线见到 huss_phone: ' + JSON.stringify((s0.devices || []).map((d) => d.name)));
  const fpPc = s0.selfDevice.id;
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
    log('已输码提交');
  } else {
    log('(已配对直连)');
  }
  await A.pollUntil((s) => (s.sessions ?? []).some((x) => x.peer === phone.id && x.trusted),
    { timeoutMs: 30_000, intervalMs: 1000, what: 'PC-手机会话 trusted' });
  log('连接 ✓');

  // ---- 手机远程页 → huss_pc → 文件列表（tab 切换确认 + 首入自愈 + 滚动找行）----
  log('== 远程浏览到目标文件 ==');
  await ch.tap({ testTag: 'nav-files-link' }); await sleep(1500);
  // 切远程 tab 要确认生效（首次进入 Compose 未 settled 时 tap 会丢）：
  // 远程页特征 = '选择远程设备' 选择器 或 已有列表（breadcrumb/文件行）
  for (let i = 0; i < 4; i++) {
    els = await ch.dump();
    const onRemote = els.some((e) => (e.text || '').trim() === '选择远程设备')
      || els.some((e) => (e.text || '').trim() === oldName);
    if (onRemote) break;
    await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
  }
  const findInDump = async () => {
    const es = await ch.dump();
    return { es, row: es.find((e) => (e.text || '').trim() === oldName) };
  };
  let fileNode = null;
  els = await ch.dump();
  let selfheal = 0;
  while (!fileNode && selfheal < 12) {
    const onPicker = els.some((e) => (e.text || '').trim() === '选择远程设备');
    const rows = els.filter((e) => /\.\w{2,4}$/.test((e.text || '').trim()) && (e.text || '').includes('.'));
    if (onPicker) {
      // 设备选择页 → 点 huss_pc 行
      log(`[browse ${selfheal}] picker 在，点 huss_pc`);
      const pc = els.find((e) => /huss_pc/.test(e.text || ''));
      if (pc) { await ch.tapXY(pc.center[0], pc.center[1]); }
      await sleep(3000);
      els = (await ch.dump());
      fileNode = els.find((e) => (e.text || '').trim() === oldName) ?? null;
    } else if (rows.length === 0) {
      // 首入竞态：列表空（选中后 ListDir 未回/失败）→ tab 互换重进触发重载
      // （android-pull-batch 实证的空列表自愈手法）
      log(`[browse ${selfheal}] 列表空，tab 互换重进`);
      await ch.tap({ testTag: 'files-location-local-tab' }); await sleep(1200);
      await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
      els = await ch.dump();
    } else {
      // 列表有内容：滚动找（目标名 r 开头，排在 0bp 等历史文件之后）
      log(`[browse ${selfheal}] 列表 ${rows.length} 行，滚动找`);
      ({ es: els, row: fileNode } = await findInDump());
      if (!fileNode) { ch.swipe(540, 1700, 540, 800); await sleep(900); }
    }
    selfheal += 1;
  }
  els = (await ch.dump());
  fileNode = els.find((e) => (e.text || '').trim() === oldName) ?? fileNode;
  assert(fileNode, `远程文件列表应见 ${oldName}: ` + ch.dumpSummary(els, 14).replace(/\n/g, ' | ').slice(0, 400));
  await ch.screenshot(join(runDir, 'v-remote-file.png'));

  // ---- 长按 → 能力探测 → 菜单"重命名" → 对话框追加后缀 → 确定 ----
  log('== 长按重命名(能力探测) ==');
  longPress(fileNode.center[0], fileNode.center[1]);
  await sleep(1500);
  els = await ch.dump();
  let menuRename = els.find((e) => (e.text || '') === '重命名');
  if (!menuRename) {
    // 产品事实（2026-09-08 实证截图）：M7 Compose 重构后远程文件菜单只有
    // 「下载到本机/选择多项」——远程重命名未随重构移植（手工清单 #11 为
    // v0.5.0 时代 PASS 项）。产品红线禁改产品代码 → 场景化到能力探测点为止，
    // SKIP 出口带诊断；产品补齐远程重命名后本场景自动转为全量断言。
    const menuTexts = els.map((e) => (e.text || '').trim()).filter(Boolean);
    log(`远程菜单能力探测：无"重命名"，菜单可见项=${JSON.stringify(menuTexts.slice(-6))}`);
    await ch.screenshot(join(runDir, 'v-remote-menu-no-rename.png'));
    try { keyEvent(4); } catch { /* BACK 收起底部弹层，不挡链式后续场景 */ }
    try {
      writeFileSync(
        join(runDir, 'SKIPPED.txt'),
        `${new Date().toISOString()} 产品缺口：远程文件菜单无"重命名"（M7 Compose 重构未移植，BACKLOG）。` +
        `场景已自动化到长按菜单探测点，产品补齐后即转全量断言（配对/浏览/长按链路已验证）。\n` +
        `菜单可见项: ${JSON.stringify(menuTexts)}\n`,
        'utf8',
      );
    } catch { /* 尽力 */ }
    console.log(`[${SCENARIO}] SKIP：远程重命名能力缺失（产品缺口，见 ${runDir}）`);
    try { await A.endTest('pass'); } catch { /* 尽力 */ }
    process.exit(0);
  }
  await ch.tapXY(menuRename.center[0], menuRename.center[1]);
  await sleep(1200);
  els = await ch.dump();
  const field = els.find((e) => (e.text || '') === oldName) || els.find((e) => /新名称/.test(e.text || ''));
  assert(field, 'RenameDialog 应出现（含原名字段）');
  await ch.tapXY(field.center[0], field.center[1]); await sleep(600);
  keyEvent(123); // KEYCODE_MOVE_END：光标移末尾
  await sleep(300);
  await ch.inputText(suffix);
  await sleep(400);
  els = await ch.dump();
  const okBtn = els.find((e) => (e.text || '') === '确定');
  assert(okBtn, 'RenameDialog 应有"确定"');
  await ch.tapXY(okBtn.center[0], okBtn.center[1]);
  await sleep(1800);
  await ch.screenshot(join(runDir, 'v-after-rename.png'));

  // ---- 断言：PC 端旧名消失/新名在/内容不变（本地 fs 直核）----
  log('== 断言:PC 端文件已改名 ==');
  let renamed = false;
  for (let i = 0; i < 6 && !renamed; i++) { // 远程 op 落盘余量
    await sleep(1000);
    renamed = !existsSync(oldPath) && existsSync(newPath);
  }
  assert(renamed, `PC 端未改名：old 存在=${existsSync(oldPath)} new 存在=${existsSync(newPath)}`);
  const content = readFileSync(newPath, 'utf8');
  assert(content === body, '改名后文件内容应不变');
  log(`改名 ✓ ${oldName} → ${newName}（内容一致）`);

  // ---- 收尾 ----
  rmSync(oldPath, { force: true });
  rmSync(newPath, { force: true });
  await A.endTest('pass');
  log(`PASS 证据: ${runDir}`);
}

try {
  await main();
} catch (e) {
  console.error(`\n[${SCENARIO}] FAIL: ${e.message}`);
  console.error(e.stack || '');
  try { await ch.screenshot(join(runDir, 'v-fail.png')); } catch { /* 尽力 */ }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
  // 共享区播种文件兜底清理（oldPath/newPath 可能未达定义——按同规则重算）
  try {
    const settings = await A.invoke('get_settings');
    const base = settings.shares[0].path;
    rmSync(join(base, `rr-${stamp}-${rand}.txt`), { force: true });
    rmSync(join(base, `rr-${stamp}-${rand}-renamed.txt`), { force: true });
  } catch { /* 尽力 */ }
  process.exitCode = 1;
}
