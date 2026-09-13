// M2-T7 → P3-T3：安卓批量拉取场景（PC 共享区 → 手机远程浏览 → 多选批量下载）。
// 断言面：PC 端 source-pull 卡终态 + 手机端文件落盘（run-as 列 downloads）+ Bridge TransferDone。
// 交互模型（P3-T3 起真多选）：文件行长按 → 「选择多项」进多选态 → 「全选」→
// 「下载(3)」批量按钮 → pull_files 多路径聚合单卡（FFI N1-T3 语义：一次调用
// 逐文件顺序拉，手机端 1 张聚合卡累计终态；PC 端仍逐文件 source-pull 卡）。
// 播种改走专属子目录（share/pull-<stamp>/）：全选=恰好 3 文件，断言确定性。
import { mkdirSync, writeFileSync, readdirSync, unlinkSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import createAdb from '../lib/adb.mjs';

const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const ch = createAdb(targets.huss_phone.adb);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const runDir = join(e2eRoot(), 'reports', `android-pull-batch-${stamp}`);
mkdirSync(runDir, { recursive: true });
const log = (m) => console.log(m);
const sh = (c) => { try { return execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', c], { encoding: 'utf8', timeout: 20000 }); } catch (e) { return 'ERR ' + e.message.slice(0, 150); } };
const longPress = (x, y) => execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', 'input', 'swipe', String(x), String(y), String(x), String(y), '900']);
function assert(cond, msg) { if (!cond) throw new Error(msg); }

// 3 个唯一文件（100/50/200 KB），播种进专属子目录
const SUBDIR = `pull-${stamp}`;
const files = [['pa', 100], ['pb', 50], ['pc', 200]].map(([n, kb]) => {
  const name = `${n}-${stamp}.bin`;
  const p = join(e2eRoot(), 'fixtures', 'runs', `pull-batch-${stamp}`, name);
  mkdirSync(join(p, '..'), { recursive: true });
  const b = Buffer.alloc(kb * 1024); b.write(`PULL ${stamp} ${n}`, 0, 'utf8'); b.fill(0x39, 64);
  writeFileSync(p, b);
  return { name, path: p, bytes: kb * 1024 };
});

try {
  console.log('== 准备 ==');
  for (const t of [A]) assert((await t.health()).bridgeReady, 'A bridgeReady');
  // 清活动引擎（链式防残留）
  try {
    const { reset } = await import('../lib/reset.mjs');
    await reset(2, [A]);
  } catch (e) { console.log('reset(2) 跳过:', e.message.slice(0, 60)); }

  const settings = await A.invoke('get_settings', {});
  const sharePath = settings.shares[0].path;
  // 清历史 pull-batch 播种残留（本场景自产的根目录散文件 + 子目录）
  for (const f of readdirSync(sharePath)) {
    if (/^p[abc]-[a-z0-9]+\.bin$/.test(f)) { try { unlinkSync(join(sharePath, f)); } catch { } }
    if (/^pull-[a-z0-9]+$/.test(f)) { try { rmSync(join(sharePath, f), { recursive: true, force: true }); } catch { } }
  }
  const seedDir = join(sharePath, SUBDIR);
  mkdirSync(seedDir, { recursive: true });
  for (const f of files) writeFileSync(join(seedDir, f.name), readFileSync(f.path));
  console.log(`PC 播种 3 文件 → ${SUBDIR}/ ✓`);

  const s0 = await A.state();
  const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
  assert(phone, 'A 应在线见到手机');
  // 已配对直连
  try { await A.invoke('connect', { fingerprint: phone.id }); } catch { }
  await A.pollUntil((st) => (st.sessions ?? []).some((x) => x.peer === phone.id),
    { timeoutMs: 20_000, intervalMs: 800, what: 'PC-手机会话' });
  console.log('PC-手机会话 ✓');

  // 手机 UI：文件页 → 远程 → 选 PC → 进播种子目录
  ch.wake(); ch.setStayOn(true); await sleep(1000);
  // MIUI 系统页干扰兜底(屏幕使用时间看板/权限框):若首 dump 无本应用 nav,回桌面重拉应用
  let navEls = await ch.dump().catch(() => []);
  if (!navEls.some((e) => (e.testTag || '').startsWith('nav-'))) {
    execFileSync(ch.adbPath, ['-s', ch.serial, 'shell', 'input', 'keyevent', '4']); // BACK
    await sleep(800);
    ch.launch(); await sleep(3000);
    navEls = await ch.dump().catch(() => []);
  }
  assert(navEls.some((e) => (e.testTag || '').startsWith('nav-')), '应用主 UI 应可见');
  await ch.tap({ testTag: 'nav-files-link' }); await sleep(1200);
  await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
  // 首入竞态自愈：tab 互换
  await ch.tap({ testTag: 'files-location-local-tab' }); await sleep(1200);
  await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
  let els = await ch.dump();
  const pcEntry = els.find((e) => /huss_pc/.test(e.text || ''));
  assert(pcEntry, '远程页应见 huss_pc');
  ch.tapXY(pcEntry.center[0], pcEntry.center[1]); await sleep(2200);
  // 首入竞态自愈循环:picker 在→点 huss_pc;列表有内容但子目录不在屏→滚动找
  // (共享区根残留多,p* 排在 m* 之后,首屏看不到);列表空→tab 互换重进触发重载
  els = await ch.dump();
  let dirRow = els.find((e) => (e.text || '').trim() === SUBDIR);
  let selfheal = 0;
  while (!dirRow && selfheal < 10) {
    const onPicker = els.some((e) => (e.text || '').trim() === '选择远程设备');
    const hasRows = els.some((e) => /\.(bin|txt)$/.test((e.text || '').trim()));
    if (onPicker) {
      const pc2 = els.find((e) => /huss_pc/.test(e.text || ''));
      if (pc2) { ch.tapXY(pc2.center[0], pc2.center[1]); await sleep(2200); }
    } else if (hasRows) {
      ch.swipe(540, 1700, 540, 800); await sleep(900); // 列表往下翻
    } else {
      await ch.tap({ testTag: 'files-location-local-tab' }); await sleep(1200);
      await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
      const pc2 = (await ch.dump().catch(() => [])).find((e) => /huss_pc/.test(e.text || ''));
      if (pc2) { ch.tapXY(pc2.center[0], pc2.center[1]); await sleep(2200); }
    }
    els = await ch.dump();
    dirRow = els.find((e) => (e.text || '').trim() === SUBDIR);
    selfheal += 1;
  }
  assert(dirRow, `远程列表应见子目录 ${SUBDIR}`);
  ch.tapXY(dirRow.center[0], dirRow.center[1]); await sleep(2200);
  // 目录内：等 3 目标文件全可见（复用 tab 互换自愈）
  els = await ch.dump();
  let found = files.filter((f) => els.some((e) => (e.text || '').trim() === f.name)).length;
  selfheal = 0;
  while (found < files.length && selfheal < 4) {
    await ch.tap({ testTag: 'files-location-local-tab' }); await sleep(1200);
    await ch.tap({ testTag: 'files-location-remote-tab' }); await sleep(1500);
    const pc2 = (await ch.dump().catch(() => [])).find((e) => /huss_pc/.test(e.text || ''));
    if (pc2) { ch.tapXY(pc2.center[0], pc2.center[1]); await sleep(2200); }
    const sub2 = (await ch.dump().catch(() => [])).find((e) => (e.text || '').trim() === SUBDIR);
    if (sub2) { ch.tapXY(sub2.center[0], sub2.center[1]); await sleep(2200); }
    els = await ch.dump();
    found = files.filter((f) => els.some((e) => (e.text || '').trim() === f.name)).length;
    selfheal += 1;
  }
  console.log(`自愈后可见 ${found}/3`);

  // ===== P3-T3 真多选：长按 → 选择多项 → 全选 → 下载(3) =====
  let row = null;
  for (let i = 0; i < 6 && !row; i++) {
    els = await ch.dump();
    row = els.find((e) => (e.text || '').trim() === files[0].name);
    if (!row) { ch.swipe(540, 1700, 540, 800); await sleep(800); }
  }
  assert(row, `远程列表应有 ${files[0].name}`);
  longPress(row.center[0], row.center[1]); await sleep(1500);
  els = await ch.dump();
  const multi = els.find((e) => (e.text || '').trim() === '选择多项');
  assert(multi, `长按 ${files[0].name} 应出「选择多项」菜单项`);
  ch.tapXY(multi.center[0], multi.center[1]); await sleep(1200);

  // 多选态：长按项已勾选，底部栏出现「全选」+「下载(1)」
  els = await ch.dump();
  const allBtn = els.find((e) => (e.text || '').trim() === '全选');
  assert(allBtn, '多选态底部栏应有「全选」按钮');
  assert(els.some((e) => /^下载\(1\)$/.test((e.text || '').trim())), '多选态应有「下载(1)」按钮');
  await ch.screenshot(join(runDir, 'v-multiselect-1.png'));

  // 全选 → 3 文件全勾选
  ch.tapXY(allBtn.center[0], allBtn.center[1]); await sleep(1000);
  els = await ch.dump();
  assert(els.some((e) => /已选 3 项/.test(e.text || '')), `全选后应显示「已选 3 项」: ${(els.map((e) => e.text).filter(Boolean).slice(0, 20)).join('|').slice(0, 150)}`);
  const dlAll = els.find((e) => /^下载\(3\)$/.test((e.text || '').trim()));
  assert(dlAll, '全选后应有「下载(3)」批量按钮');
  await ch.screenshot(join(runDir, 'v-multiselect-3.png'));
  ch.tapXY(dlAll.center[0], dlAll.center[1]);
  console.log('多选 3 文件批量下载已发起 ✓');
  await sleep(2000);

  // 断言 1：PC 端 source-pull 卡终态（聚合拉取在 PC 侧仍逐文件建卡，字节核对）
  await A.pollUntil((st) => {
    const mine = (st.transfers || []).filter((t) => t.direction === 'pull'
      && files.some((f) => (t.name || '').includes(f.name)));
    return mine.length === 3 && mine.every((t) => ['done', 'failed', 'interrupted'].includes(t.state))
      ? mine : false;
  }, { timeoutMs: 120_000, intervalMs: 1000, what: 'PC 3 张 source-pull 卡终态' });
  const s2 = await A.state();
  const cards = (s2.transfers || []).filter((t) => t.direction === 'pull'
    && files.some((f) => (t.name || '').includes(f.name)));
  for (const c of cards) {
    assert(c.state === 'done', `PC 卡 ${c.name} state=${c.state}`);
    assert(c.bytesDone === c.bytesTotal, `PC 卡 ${c.name} done(${c.bytesDone})!=total(${c.bytesTotal})`);
  }
  console.log('PC 3 卡全部 done，字节一致 ✓');

  // 断言 2：手机落盘（run-as 列私有 downloads；批量拉取按文件名落平铺根）
  await sleep(2000);
  // 落盘根=ffi setInboxDir 注入的 files/downloads(Download/LocalTrans 为公共目录别名,
  // 实测私有根才稳定可查)
  const ls = sh('run-as com.localtrans.app ls files/downloads');
  for (const f of files) assert(ls.includes(f.name), `手机缺落盘文件 ${f.name}: ${ls.slice(0, 150)}`);
  console.log('手机 3 文件落盘 ✓');

  // 断言 3：手机传输页 1 张聚合卡（"3 files"，pull_files 多路径聚合单卡语义）
  await ch.tap({ testTag: 'nav-transfers-link' }); await sleep(1500);
  // 拉取完成即终态 → 直接收进折叠的历史区，先展开
  await ch.tap({ testTag: 'transfers-history-fold-btn' }); await sleep(1200);
  els = await ch.dump();
  const aggCard = els.find((e) => (e.text || '').trim() === '3 files');
  assert(aggCard, '手机传输页(历史展开)应见 1 张聚合卡「3 files」');
  await ch.screenshot(join(runDir, 'v-phone-pull-batch.png'));
  console.log('PASS：3 文件多选批量拉取全链路（PC 卡 done×3 + 手机落盘×3 + 手机传输页聚合卡「3 files」）');
  console.log('证据:', runDir);
} catch (e) {
  try { ch.screenshot(join(runDir, 'v-fail.png')); } catch { }
  console.error('FAIL:', e.message.slice(0, 200));
  process.exitCode = 1;
}
