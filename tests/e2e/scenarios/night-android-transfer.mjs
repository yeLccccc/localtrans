// N1-T6 / M8-a：PC↔Android 真实传输场景（配对→PC推手机→手机批量拉PC=验证T3修复）。
// 设备：A=huss_pc(test-api)、手机=huss_phone(adb)。信任表含旧指纹,需真实重新配对。
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import { reset } from '../lib/reset.mjs';
import createAdb from '../lib/adb.mjs';

const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const runDir = join(e2eRoot(), 'reports', `android-xfer-${stamp}`);
mkdirSync(runDir, { recursive: true });

function assert(cond, msg) { if (!cond) throw new Error(msg); }
const log = (m) => console.log(m);
const findText = (els, re) => els.find((e) => re.test(e.text) || re.test(e.contentDesc));

try {
  // ---------- 准备 ----------
  log('== 准备:双端就绪 + 手机重装包/亮屏/自动同意 ==');
  assert((await A.health()).bridgeReady, 'A bridgeReady');
  ch.wake(); ch.setStayOn(true); await sleep(1200);
  await ch.install('C:/Users/<user>/Desktop/work/localTrans/android/app/build/outputs/apk/debug/app-debug.apk');
  ch.clearLogcat(); ch.launch();
  const banner = await ch.waitForBanner(30_000);
  log('banner ✓ ' + banner.slice(0, 60));
  await sleep(2500);

  // 手机开自动同意钩子(设置页,debug 区块在折叠区下方——先滚动到底)
  await ch.tap({ testTag: 'nav-settings-link' }); await sleep(1200);
  let hook = null;
  for (let i = 0; i < 4 && !hook; i++) {
    const els = await ch.dump();
    hook = els.find((e) => e.testTag === 'settings-test-auto-consent-toggle');
    if (!hook) { ch.swipe(540, 1750, 540, 750); await sleep(700); }
  }
  assert(hook, '设置页(滚动后)应有自动同意开关');
  if (!hook.checked) { await ch.tap({ testTag: 'settings-test-auto-consent-toggle' }); await sleep(600); }
  log('自动同意钩子 ✓');
  await ch.tap({ testTag: 'nav-devices-link' }); await sleep(1000);

  // A 侧准备:清卡 + 找手机当前指纹 + 播种共享区 3 文件(手机拉取用)
  await reset(2, [A]); // 进程重启清活动引擎(链式场景下前站可能留活动卡)
  const s0 = await A.state();
  // factoryReset 后手机广播名回出厂"我的手机";改名后的 huss_phone 也接受
  const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
  assert(phone, 'A 应在线见到 huss_phone: ' + JSON.stringify((s0.devices || []).map(d => d.name)));
  const fpPhone = phone.id;
  log('手机当前指纹 ' + fpPhone.slice(0, 8));
  const settings = await A.invoke('get_settings', {});
  const sharePath = settings.shares[0].path;
  const pullFiles = [`pa-${stamp}.bin`, `pb-${stamp}.bin`, `pc-${stamp}.bin`];
  const sizes = [100 * 1024, 50 * 1024, 200 * 1024];
  pullFiles.forEach((n, i) => {
    const b = Buffer.alloc(sizes[i]); b.write(`PULL ${stamp} ${n}`, 0, 'utf8'); b.fill(0x33, 64);
    writeFileSync(join(sharePath, n), b);
  });
  log('PC 共享区播种 3 文件(350KB) ✓');

  // ---------- 连接/配对:手机点 PC 卡。已配对→直连;未配对→同意门+输码。
  // 幂等化(run-all 链式下上站可能已配对,配对是手段不是断言对象)
  log('== 连接/配对(手机发起) ==');
  const fpPc = s0.selfDevice.id;
  await ch.tap({ testTag: `device-card-${fpPc}` });
  await sleep(1500);
  // 手机侧:已直连(无等待提示)或等对方同意
  let els = await ch.dump();
  const waiting = els.find((e) => /正在等待|等待对方/.test(e.text));
  log('手机状态: ' + (waiting ? waiting.text : '(可能已直连)'));
  let paired = false;
  if (waiting) {
    // 未配对:走完整配对流
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
    await A.screenshot(join(runDir, 'v-pc-code.png'));
    log('配对码 ' + code);
    await sleep(800);
    els = await ch.dump();
    const input = els.find((e) => e.testTag === 'code-input');
    assert(input, '手机应有输码框(code-input)');
    await ch.tap({ testTag: 'code-input' });
    await ch.inputText(code);
    await ch.tap({ testTag: 'btn-submit' });
    paired = true;
    log('已输码提交');
  } else {
    // 已直连:验证手机屏上"已连接"徽章
    els = await ch.dump();
    assert(els.some((e) => e.text === '已连接'), '手机应显示已连接(或回设备页确认)');
    paired = true;
  }
  await A.pollUntil((st) => (st.sessions ?? []).some((x) => x.peer === fpPhone),
    { timeoutMs: 30_000, intervalMs: 1000, what: '会话建立' });
  log('连接 ✓ 会话已建立' + (paired ? '' : '(已配对直连)'));

  // ---------- 推送 PC→手机(2MB 单文件,真实 OfferSheet) ----------
  log('== 推送 PC→手机 ==');
  const pushName = `to-phone-${stamp}.bin`;
  const pushPath = join(runDir, pushName);
  { const b = Buffer.alloc(2 * 1024 * 1024); b.write(`PUSH ${stamp}`, 0, 'utf8'); b.fill(0x44, 64); writeFileSync(pushPath, b); }
  const cardId = await A.invoke('push_files', { fingerprint: fpPhone, local_paths: [pushPath] });
  // 手机 OfferSheet:优先 testid;回落按文本找。
  // 注意 Compose Button 的 a11y 树:可点击节点(android.view.View,文本空)与
  // 文本子节点(TextView"接收",clickable=false)是两个节点——"同节点 clickable&&text"
  // 永假,这是重装后弹窗"丢失"假阴性的真根因(2026-09-07 探针+截图实证:事件链
  // 与弹窗渲染全程正常)。文本节点落在按钮 bounds 内,按其中心点按即命中按钮。
  let accepted = false;
  for (let i = 0; i < 25 && !accepted; i++) {
    await sleep(800);
    const offerEls = await ch.dump();
    const btn = offerEls.find((e) => e.testTag === 'offer-accept-btn')
      || offerEls.find((e) => /接收|接受|允许/.test((e.text || '') + (e.contentDesc || '')));
    if (!btn) continue;
    // tap(sel) 只收选择器,坐标点按走 tapXY;文本节点中心在按钮 bounds 内,等价命中
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
  // 手机传输页应见完成卡
  await ch.tap({ testTag: 'nav-transfers-link' }); await sleep(1500);
  await ch.screenshot(join(runDir, 'v-phone-transfers.png'));
  log('推送 ✓ done ' + (pushed.value.bytesDone / 1048576).toFixed(1) + 'MB');

  // ---------- 手机批量拉 PC(T3 真机验证:3 文件 → 恒 1 卡) ----------
  log('== 手机批量拉取 PC(T3) ==');
  await ch.tap({ testTag: 'nav-files-link' }); await sleep(1200);
  await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
  els = await ch.dump();
  log('远程页摘要: ' + ch.dumpSummary(els, 14).replace(/\n/g, ' | ').slice(0, 400));
  await ch.screenshot(join(runDir, 'v-phone-remote-1.png'));
  // 自适应:依次找 设备选择(huss_pc) → 共享区/文件
  const pcCard = els.find((e) => e.text === 'huss_pc' || e.testTag === `device-card-${'7b00fc65'}` || (e.text || '').includes('huss_pc'));
  assert(pcCard, '远程页应见 huss_pc');
  await ch.tapXY(pcCard.center[0], pcCard.center[1]); await sleep(2000);
  els = await ch.dump();
  log('选中后摘要: ' + ch.dumpSummary(els, 16).replace(/\n/g, ' | ').slice(0, 440));
  await ch.screenshot(join(runDir, 'v-phone-remote-2.png'));
  // TODO(迭代):根据实际 UI 补:共享区选择→文件多选→下载
  // 首轮先把结构带回来,后续迭代补齐交互
  log('（首轮侦查完成——远程浏览交互待按实际 UI 补齐）');

  log('PASS(推送+配对段),证据: ' + runDir);
} catch (e) {
  try { await ch.screenshot(join(runDir, 'v-fail.png')); } catch { }
  console.error('FAIL:', e.message);
  process.exitCode = 1;
} finally {
  try { rmSync(join(runDir, `to-phone-${stamp}.bin`), { force: true }); } catch { }
}
