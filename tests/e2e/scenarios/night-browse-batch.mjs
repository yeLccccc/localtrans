// N1-T4/T5 真机验证：浏览页批次下载(1批次→1父卡片) + 本地排序 + 删刷新按钮。
// 前置：B(huss_laptop) 共享区播种 3 个小文件(ssh)；A(huss_pc) 走真实 UI。
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import { reset } from '../lib/reset.mjs';
import { sshExec } from '../lib/deploy.mjs';

const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const B = new Target('huss_laptop', targets.huss_laptop, null);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stamp = Date.now().toString(36);
const runDir = join(e2eRoot(), 'reports', `browse-batch-${stamp}`);
const SHARE = 'C:/Users/huss_laptop/localtrans-test/share';
const fa = `ta-${stamp}.bin`, fb = `tb-${stamp}.bin`, fc = `tc-${stamp}.bin`;

function assert(cond, msg) { if (!cond) throw new Error(msg); }

try {
  mkdirSync(runDir, { recursive: true });
  console.log('== 准备 ==');
  for (const t of [A, B]) assert((await t.health()).bridgeReady, `${t.name} bridgeReady`);
  await reset(1, [A, B]);

  // B 共享区播种 3 文件（大小 30/10/20 字节可区分排序）
  const mk = (n, size) =>
    `powershell -NoProfile -Command "[IO.File]::WriteAllBytes('${SHARE}/${n}', [byte[]]::new(${size}))"`;
  await sshExec(targets.huss_laptop, mk(fa, 30), { ignoreCode: true });
  await sshExec(targets.huss_laptop, mk(fb, 10), { ignoreCode: true });
  await sshExec(targets.huss_laptop, mk(fc, 20), { ignoreCode: true });
  console.log('播种:', fa, fb, fc);

  const fpB = (await B.state()).selfDevice.id;
  // 等发现表先见到 B(部署刚重启,广播需要数秒)
  await A.pollUntil((st) => (st.devices ?? []).some((d) => d.id === fpB && d.online),
    { timeoutMs: 30_000, intervalMs: 1000, what: 'A 发现 B' });
  await sleep(1500);
  await A.invoke('connect', { fingerprint: fpB });
  await A.pollUntil((s) => (s.sessions ?? []).length > 0, { timeoutMs: 20_000, what: '会话建立' });

  // ---------- T5: 删刷新按钮 + 排序控件 ----------
  console.log('== T5: 浏览页 UI 驱动 ==');
  await A.uiNavigate('/browse');
  await A.uiWait(`[data-testid="browse-device-chip-${fpB}"]`, 15_000);
  await A.uiClick(`[data-testid="browse-device-chip-${fpB}"]`);
  // 共享区需点击选中后才拉文件列表
  await A.uiWait('.share-item', 15_000);
  await A.uiClick('.share-item');
  await A.uiWait(`[data-testid="browse-file-check-${fa}"]`, 20_000);

  const flat = JSON.stringify(await A.uiTree());
  assert(!flat.includes('browse-shares-refresh-btn'), '共享区刷新按钮应已删除');
  assert(!flat.includes('browse-files-refresh-btn'), '文件区刷新按钮应已删除');
  assert(flat.includes('browse-sort-control'), '排序控件应存在');
  console.log('T5 ✓ 刷新按钮×2 已删 + 排序控件在');

  // 名称升序（默认）：a<b<c
  let order = await A.uiText('.files-list');
  let iA = order.indexOf(fa), iB = order.indexOf(fb), iC = order.indexOf(fc);
  assert(iA >= 0 && iA < iB && iB < iC, `名称升序错误: ${[iA, iB, iC]}`);
  // 大小升序：10<20<30 → b,c,a
  await A.uiClick('[data-testid="browse-sort-size"]');
  order = await A.uiText('.files-list');
  iA = order.indexOf(fa); iB = order.indexOf(fb); iC = order.indexOf(fc);
  assert(iB >= 0 && iB < iC && iC < iA, `大小升序错误: ${[iB, iC, iA]}`);
  // 大小降序：a,c,b
  await A.uiClick('[data-testid="browse-sort-dir"]');
  order = await A.uiText('.files-list');
  iA = order.indexOf(fa); iB = order.indexOf(fb); iC = order.indexOf(fc);
  assert(iA >= 0 && iA < iC && iC < iB, `大小降序错误: ${[iA, iC, iB]}`);
  await A.screenshot(join(runDir, 'v-sort-size-desc.png'));
  console.log('T5 ✓ 本地排序 名称/大小+升降 正确');

  // ---------- T4: 批次下载 1 父卡片 ----------
  console.log('== T4: 批次下载 ==');
  await A.uiClick(`[data-testid="browse-file-check-${fa}"]`);
  await A.uiClick(`[data-testid="browse-file-check-${fb}"]`);
  await A.uiClick(`[data-testid="browse-file-check-${fc}"]`);
  await A.uiClick('[data-testid="browse-download-selected-btn"]');

  await A.pollUntil((s) => {
    const hit = (s.transfers || []).find((t) => (t.name || '').includes(fa)
      && ['done', 'failed', 'interrupted'].includes(t.state));
    return hit ?? false;
  }, { timeoutMs: 60_000, intervalMs: 500, what: '批次卡终态' });
  await sleep(1200);
  const s = await A.state();
  const pulls = (s.transfers || []).filter((t) => t.direction === 'pull' && (t.name || '').includes(stamp));
  assert(pulls.length === 1, `应恒 1 张父卡,实际 ${pulls.length}: ${JSON.stringify(pulls.map(p => p.name))}`);
  assert(pulls[0].state === 'done', `父卡应 done,实际 ${pulls[0].state}`);
  assert(pulls[0].bytesTotal === 60, `总量应 60,实际 ${pulls[0].bytesTotal}`);
  assert(pulls[0].bytesDone === 60, `完成量应 60,实际 ${pulls[0].bytesDone}`);
  console.log(`T4 ✓ 1 父卡 done 60/60 (card=${pulls[0].id})`);

  const dl = await sshExec(targets.huss_laptop,
    `dir /b "${SHARE}\\${fa}" "${SHARE}\\${fb}" "${SHARE}\\${fc}"`, { ignoreCode: true });
  console.log('B 落盘核对:', String(dl.out).trim().replace(/\s+/g, ' ').slice(0, 120));

  await A.uiNavigate('/transfers');
  await sleep(800);
  await A.screenshot(join(runDir, 'v-batch-parent-card.png'));
  console.log('PASS，证据:', runDir);
} finally {
  try {
    await sshExec(targets.huss_laptop,
      `powershell -NoProfile -Command "Remove-Item '${SHARE}/t?-${stamp}.bin' -ErrorAction SilentlyContinue"`,
      { ignoreCode: true });
  } catch { /* 尽力 */ }
}
