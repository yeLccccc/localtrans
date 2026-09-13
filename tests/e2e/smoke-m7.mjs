// smoke-m7: Android 接入冒烟（M7 验收——spec §9 M7: adb 通道完成同级别"看-点-断言-截图-日志"）
// 前置：USB 连接的 Android 真机（targets.local.yaml android.serial），USB 调试已授权；
//       debug APK 已构建（android/app/build/outputs/apk/debug/app-debug.apk）。
// 用法：node smoke-m7.mjs
// 可选：huss_pc 桌面 test-api 在跑（127.0.0.1:39872）时，附加"手机发现桌面"互发现断言；
//       不可达或未发现 → SKIP（不算 FAIL）。
import { mkdirSync, writeFileSync } from 'node:fs';
import { loadTargets } from './lib/config.mjs';
import { createAdbChannel } from './lib/adb.mjs';

const targets = loadTargets();
const APK = new URL('../../android/app/build/outputs/apk/debug/app-debug.apk', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1');

const adb = createAdbChannel({ serial: targets.huss_phone.serial });

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const step = async (name, fn) => {
  try { const detail = await fn(); results.push(['PASS', name, detail]); console.log(`  PASS  ${name}${detail ? '  ' + detail : ''}`); }
  catch (e) { results.push(['FAIL', name, e.message]); console.log(`  FAIL  ${name}  ${e.message}`); }
};
const skip = (name, why) => { results.push(['SKIP', name, why]); console.log(`  SKIP  ${name}  ${why}`); };
const assert = (c, msg) => { if (!c) throw new Error(msg); };

// M7 铺设的已知 testTag（断言 dump 里 tag 以 resource-id 暴露）
const KNOWN_TAGS = [
  'nav-devices-link', 'nav-files-link', 'nav-transfers-link', 'nav-settings-link',
  'devices-hidden-toggle', 'devices-local-ip-chip', 'devices-manual-add-open-btn',
];

const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 15);
const outDir = new URL(`./reports/m7-${ts}/`, import.meta.url).href.replace('file:///', '');
mkdirSync(outDir, { recursive: true });

console.log(`[smoke-m7] Android 接入冒烟 开始（serial=${adb.serial}, adb=${adb.adbPath}）`);

await step('安装 debug APK', async () => {
  return (await adb.install(APK)).replace(/\s+/g, ' ');
});

let banner;
await step('launch + waitForBanner（LT-BANNER 横幅）', async () => {
  adb.forceStop();
  adb.clearLogcat(); // 清缓冲，避免匹配上一轮残留横幅
  adb.launch();
  banner = await adb.waitForBanner(30000);
  assert(banner.includes('LT-BANNER'), '横幅缺 LT-BANNER 特征串');
  assert(banner.includes('version='), '横幅缺 version');
  assert(/version=0\.\d+\.\d+/.test(banner), `横幅版本异常: ${banner}`);
  writeFileSync(outDir + 'banner.txt', banner + '\n');
  return banner.slice(Math.max(0, banner.indexOf('LT-BANNER')));
});

await step('启动截图留证', async () => {
  const s = adb.screenshot(outDir + '01-launch-devices.png');
  return `${s.path} (${s.bytes} bytes)`;
});

let dump1;
await step('dump 可见 ≥3 个 testTag 元素', async () => {
  dump1 = await adb.dump();
  writeFileSync(outDir + 'dump-devices.json', JSON.stringify(dump1, null, 2));
  const tagged = dump1.filter((e) => KNOWN_TAGS.includes(e.testTag));
  const detail = tagged.map((e) => e.testTag).join(',');
  assert(tagged.length >= 3, `testTag 元素仅 ${tagged.length} 个（期望 ≥3）；tag 命中: [${detail}]`);
  return `${tagged.length} 个: ${detail}`;
});

await step('tap nav-transfers-link（tab 切换）', async () => {
  await adb.tap({ testTag: 'nav-transfers-link' }, { retries: 3 });
  await wait(800); // 导航动画
  return 'tapped';
});

await step('dump 变化断言（切到传输页）', async () => {
  const d2 = await adb.dump();
  writeFileSync(outDir + 'dump-transfers.json', JSON.stringify(d2, null, 2));
  const navTransfers = d2.find((e) => e.testTag === 'nav-transfers-link');
  assert(navTransfers?.selected === true, `nav-transfers-link 未选中(selected=${navTransfers?.selected})`);
  assert(d2.some((e) => e.text === '暂无传输任务'), '传输页空态文案未出现');
  assert(!d2.some((e) => e.text === '我的设备指纹'), '设备页指纹区文案仍在（页面未切换）');
  const s = adb.screenshot(outDir + '02-transfers.png');
  return `selected=true 且空态文案在（截图 ${s.bytes}B）`;
});

await step('tap nav-devices-link（切回，无害验证）', async () => {
  await adb.tap({ testTag: 'nav-devices-link' }, { retries: 3 });
  await wait(800);
  const d3 = await adb.dump();
  assert(d3.some((e) => e.text === '我的设备指纹'), '切回设备页后指纹区文案未出现');
  return 'round-trip ok';
});

// 可选步：huss_pc 在跑时验证"手机设备页出现桌面设备名"（发现互通）
// 失败/不可达都 SKIP（M8 跨端场景才是正式验收）
let hussPcName = null;
try {
  const ctl = new AbortController();
  setTimeout(() => ctl.abort(), 3000);
  const r = await fetch(`http://${targets.huss_pc.host}:${targets.huss_pc.port}/api/state/transfers`, {
    headers: { Authorization: `Bearer ${targets.huss_pc.token}` }, signal: ctl.signal,
  });
  const snap = await r.json();
  hussPcName = snap?.data?.selfDevice?.name || null;
} catch { /* huss_pc 不在 → 可选步整体 SKIP */ }

if (hussPcName) {
  // 可选步：失败按 M7 约定降级 SKIP（M8 跨端场景才是正式验收），直接打 SKIP 不打 FAIL
  try {
    let seen = null;
    for (let i = 0; i < 5 && !seen; i++) {
      const d = await adb.dump();
      const card = d.some((e) => e.testTag.startsWith('device-card-'));
      // 卡片 testTag 与设备名文本分属父子节点（Card 空 text，名称在子 Text），分开断言
      if (card && d.some((e) => e.text.includes(hussPcName))) seen = d;
      else await wait(3000);
    }
    if (!seen) throw new Error(`设备页未见桌面设备「${hussPcName}」卡片`);
    const s = adb.screenshot(outDir + '03-discovery.png');
    results.push(['PASS', '发现互通（手机设备页出现桌面设备名）', `手机看到桌面「${hussPcName}」（截图 ${s.bytes}B）`]);
    console.log(`  PASS  发现互通（手机设备页出现桌面设备名）  手机看到桌面「${hussPcName}」`);
  } catch (e) {
    results.push(['SKIP', '发现互通（手机设备页出现桌面设备名）', `${e.message}（按 M7 约定降级 SKIP，M8 正式验收）`]);
    console.log(`  SKIP  发现互通（手机设备页出现桌面设备名）  ${e.message}`);
  }
} else {
  skip('发现互通（手机设备页出现桌面设备名）', 'huss_pc test-api 不可达或无 selfDevice.name，跳过');
}

await step('logcat LT 日志归档 + forceStop', async () => {
  const ltLogs = adb.logcat({ tagPrefix: 'LT' });
  writeFileSync(outDir + 'logcat-LT.txt', ltLogs);
  assert(ltLogs.includes('LT-BANNER'), '归档日志缺横幅');
  adb.forceStop();
  return `${ltLogs.split('\n').filter(Boolean).length} 行 LT 日志`;
});

const pass = results.filter((r) => r[0] === 'PASS').length;
const fail = results.filter((r) => r[0] === 'FAIL').length;
const skips = results.filter((r) => r[0] === 'SKIP').length;
console.log(`\n[smoke-m7] ${pass} PASS / ${fail} FAIL / ${skips} SKIP，工件: ${outDir}`);
writeFileSync(outDir + 'summary.json', JSON.stringify({ ts, serial: adb.serial, results }, null, 2));
process.exit(fail === 0 ? 0 : 1);
