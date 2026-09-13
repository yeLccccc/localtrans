// 场景：api-acceptance —— HTTP API / 远程调试工具 全能力验收（不打折扣标准）
//
// 验收面：
//   A 协议与安全：401/404/400/408 负路径、特征头、包络
//   B 观测：四页导航+tree+text+分页截图、双 store 快照、日志游标、双端截图
//   C UI 真实操作：设置改名（UI 输入→blur→持久化→invoke 复核→还原）
//   D 传输全控制：500MB 推送→B 接收弹窗（截图）→进度→暂停（停滞断言+截图）
//     →继续→done 字节断言；200MB→取消→终态→UI 删卡→卡消失
//   E Android 远程：banner 就绪→dump（见桌面设备）→tab 切换→截图
//   F 收尾：证据收割 + test/end
// 视觉验证：每个关键 UI 状态截图存 runDir，验收人（AI）逐张复核渲染正确性。
import { mkdirSync, statSync, openSync, writeSync, ftruncateSync, closeSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { ensureFixtures } from '../fixtures/gen.mjs';
import androidChannel from '../lib/adb.mjs';

const SCENARIO = 'api-acceptance';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const huss_pc = new Target('huss_pc', targets.huss_pc, journal);
const huss_laptop = new Target('huss_laptop', targets.huss_laptop, journal);
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const steps = [];
const startedAt = new Date();
let failureMsg = null;
const versions = {};
const F = (n) => `reports/${runId}/${n}`;

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

/** 大文件 fixture：头部写 runId 戳 + 截断到目标尺寸（内容唯一，规避秒传去重） */
function makeBigFile(bytes) {
  const dir = join(e2eRoot(), 'fixtures', 'runs', runId);
  mkdirSync(dir, { recursive: true });
  const p = join(dir, `big-${bytes / 1048576}mb.bin`);
  const fd = openSync(p, 'w');
  writeSync(fd, Buffer.from(`acceptance-stamp:${runId}:`));
  ftruncateSync(fd, bytes);
  closeSync(fd);
  return p;
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  // ---------- A 协议与安全 ----------
  await step('A1 认证负路径：错 Token/无 Token → 401 + 特征头', async () => {
    const bad = await fetch(`http://${targets.huss_pc.host}:${targets.huss_pc.port}/api/version`, { headers: { Authorization: 'Bearer wrong' } });
    if (bad.status !== 401) throw new Error(`错 Token 得 ${bad.status}`);
    const body = await bad.json();
    if (body?.error?.code !== 'AUTH_FAILED') throw new Error(`错误码 ${body?.error?.code}`);
    if (bad.headers.get('x-localtrans-testapi') !== '1') throw new Error('401 响应缺特征头');
    const none = await fetch(`http://${targets.huss_pc.host}:${targets.huss_pc.port}/api/version`);
    if (none.status !== 401) throw new Error(`无 Token 得 ${none.status}`);
    const h = await fetch(`http://${targets.huss_pc.host}:${targets.huss_pc.port}/api/health`);
    if (h.status !== 200) throw new Error('health 不应需要认证');
    return '401×2 + health 免认证 + 特征头 ✓';
  });

  await step('A2 路由负路径：未知路径 404 包络、非法 op 400', async () => {
    const nf = await huss_pc.api('/api/no-such-endpoint');
    if (nf.status !== 404 || nf.json?.error?.code !== 'NOT_FOUND') throw new Error(`404 异常: ${nf.status}`);
    const r = await huss_pc.api('/api/state/wait', { method: 'POST', body: JSON.stringify({ source: 'transfers', path: 'x', op: 'bogus' }) });
    if (r.status !== 400 || r.json?.error?.code !== 'BAD_REQUEST') throw new Error(`400 异常: ${r.status}`);
    return 'NOT_FOUND / BAD_REQUEST 包络 ✓';
  });

  await step('A3 等待超时语义：408 + 末次观测值', async () => {
    const r = await huss_pc.api('/api/state/wait', { method: 'POST', body: JSON.stringify({ source: 'transfers', path: 'devices.length', op: 'gte', value: 999, timeoutMs: 1200, intervalMs: 300 }) });
    if (r.status !== 408 || r.json?.error?.code !== 'WAIT_TIMEOUT') throw new Error(`408 异常: ${r.status}`);
    if (!('lastValue' in (r.json.error.detail || {}))) throw new Error('detail 缺 lastValue');
    return `408 detail.lastValue=${JSON.stringify(r.json.error.detail.lastValue)} ✓`;
  });

  // ---------- B 观测 ----------
  await step('B1 版本握手 + begin', async () => {
    for (const t of [huss_pc, huss_laptop]) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      versions[t.name] = v;
      await t.beginTest(SCENARIO);
    }
    return `双端 v${versions.huss_pc.appVersion} bridgeReady ✓`;
  });

  await step('B2 四页导航 + 元素树 + 页面截图（视觉证据）', async () => {
    const arts = [];
    for (const [path, tag] of [['/devices', 'devices'], ['/transfers', 'transfers'], ['/browse', 'browse'], ['/settings', 'settings']]) {
      await huss_pc.uiNavigate(path);
      await huss_pc.uiWait('main, [data-testid], .page, .devices-page, .settings-page', 8000).catch(() => {});
      const tree = await huss_pc.uiTree();
      if (!Array.isArray(tree) || tree.length < 3) throw new Error(`${path} tree 仅 ${tree?.length} 元素`);
      const txt = await huss_pc.uiText();
      if (!txt || txt.length < 10) throw new Error(`${path} 页面文本为空`);
      const shot = `v-01-page-${tag}.png`;
      await huss_pc.screenshot(join(runDir, shot));
      arts.push(F(shot));
      note(`${path}: tree=${tree.length} 元素, text 长度=${txt.length}`);
    }
    return { detail: '4 页均可导航/观测/截图', artifacts: arts };
  });

  await step('B3 状态观测：Rust 快照 + Pinia 四 store + 日志游标', async () => {
    const s = await huss_pc.state();
    if (s.schemaVersion !== 1 || !s.selfDevice?.id) throw new Error('TestSnapshot 异常');
    const app = await huss_pc.stateApp();
    for (const k of ['devices', 'transfers', 'settings', 'toast']) {
      if (!(k in app)) throw new Error(`state/app 缺 ${k}`);
    }
    const l1 = await huss_pc.logs({ target: 'ui' });
    const seq1 = l1.nextSeq;
    const l2 = await huss_pc.logs({ afterSeq: seq1, target: 'ui' });
    if ((l2.entries || []).some((e) => e.seq <= seq1)) throw new Error('游标续读出现重复 seq');
    return `snapshot=${s.devices.length} 设备/${s.sessions.length} 会话；ui 日志 ${l1.entries.length} 条，游标续读无重复 ✓`;
  });

  await step('B4 双端截图能力', async () => {
    await huss_pc.screenshot(join(runDir, 'v-02-huss_pc-devices.png'));
    await huss_laptop.screenshotSoft(join(runDir, 'v-03-huss_laptop-devices.png'));
    return '双端 PNG 落盘 ✓';
  });

  // ---------- C UI 真实操作（前端输入→后端持久化闭环） ----------
  await step('C1 设置页改名：UI 输入 → 持久化 → invoke 复核 → 还原', async () => {
    await huss_pc.uiNavigate('/settings');
    await huss_pc.uiWait('[testid=settings-device-name-input]', 8000);
    const before = (await huss_pc.invoke('get_settings')).device_name;
    const newName = `验收-${rand}`;
    // events:['blur']：显式派发失焦（@blur 触发保存——程序化设值无真实焦点）
    await huss_pc.uiInput('[testid=settings-device-name-input]', newName, { events: ['blur'] });
    let persisted = null;
    for (let i = 0; i < 10; i++) {
      await sleep(700);
      persisted = (await huss_pc.invoke('get_settings')).device_name;
      if (persisted === newName) break;
    }
    if (persisted !== newName) throw new Error(`UI 改名未持久化: ${JSON.stringify(persisted)}`);
    // 还原
    await huss_pc.uiInput('[testid=settings-device-name-input]', before, { events: ['blur'] });
    for (let i = 0; i < 10; i++) {
      await sleep(700);
      if ((await huss_pc.invoke('get_settings')).device_name === before) break;
    }
    const restored = (await huss_pc.invoke('get_settings')).device_name;
    if (restored !== before) throw new Error(`还原失败: ${restored}`);
    return `改名 ${before} → ${newName} → 已还原（UI 输入真实穿透到后端配置）`;
  });

  // ---------- D 传输全控制（暂停/继续/取消） ----------
  let fpB = null;
  await step('D1 L2 重置（双端进程重启清引擎态）+ 双向发现 + 会话就绪', async () => {
    // L1 不足以清引擎残留任务（实测：上轮取消的推送占活跃槽，新推送永久 pending），
    // 验收场景必须从进程级干净状态开始
    await reset(2, [huss_pc, huss_laptop]);
    // 重启清了内存 test-run 上下文——重新 begin（新 runId，覆盖 Target 记录）
    await huss_pc.beginTest(SCENARIO);
    await huss_laptop.beginTest(SCENARIO);
    // 等到**双方互见**（数量>=1 可能只是手机；WLAN 抖动下广播恢复需数秒）
    await huss_laptop.waitState({ path: 'devices.length', op: 'gte', value: 1, timeoutMs: 30_000 });
    await huss_pc.pollUntil(
      (st) => st.devices.some((d) => d.name === 'huss_laptop' && d.online),
      { timeoutMs: 40_000, intervalMs: 1500, what: 'A 见到 huss_laptop 在线' },
    );
    const sA = await huss_pc.state();
    const b = sA.devices.find((d) => d.name === 'huss_laptop');
    fpB = b.id;
    // connect 依赖发现表有此 fp——pollUntil 已保证;再防一瞬:失败短重试一次
    try {
      await huss_pc.invoke('connect', { fingerprint: fpB });
    } catch (e) {
      await new Promise((r) => setTimeout(r, 3000));
      await huss_pc.invoke('connect', { fingerprint: fpB });
    }
    await huss_pc.pollUntil((s) => s.sessions.some((x) => x.peer === fpB && x.trusted), { timeoutMs: 15_000, what: '会话建立' });
    return '发现 + 会话 trusted ✓';
  });

  const big1 = makeBigFile(500 * 1048576);
  let pauseCaught = false;
  await step('D2 500MB 推送：B 接收弹窗（截图）→ 进度 → UI 暂停 → 停滞断言 → 继续 → done', async () => {
    ensureFixtures();
    await huss_pc.step('push-500mb');
    await huss_pc.invoke('push_files', { fingerprint: fpB, local_paths: [big1] });
    await huss_laptop.dismissResumePrompt();
    await huss_laptop.dismissResumePrompt();
    await huss_laptop.uiWait('.offer-modal', 20_000);
    await huss_laptop.screenshotSoft(join(runDir, 'v-04-huss_laptop-offer-modal.png')); // 接收弹窗视觉证据
    await huss_laptop.uiClick('.offer-modal .btn-success');

    // 按 bytesTotal 定位本次推送的活动卡（占位卡/历史卡都不是断言对象）
    await huss_pc.uiNavigate('/transfers');
    await huss_pc.uiWait('[testid^=transfer-item-]', 15_000);
    const SIZE = statSync(big1).size;
    const hit = await huss_pc.pollUntil(
      (s) => s.transfers.find((t) => t.bytesTotal === SIZE && t.state === 'active' && t.bytesDone > 20 * 1048576)
        ?? false,
      { timeoutMs: 60_000, intervalMs: 300, what: '500MB 活动卡进度 >20MB' },
    );
    const cardId = hit.value.id;
    note(`目标卡 ${cardId}，暂停前进度 ${hit.value.bytesDone}/${SIZE}`);
    await huss_pc.screenshot(join(runDir, 'v-05-huss_pc-progress.png'));

    // UI 暂停（卡内作用域选择器）→ 同卡停滞断言
    try {
      await huss_pc.uiWait(`[testid=transfer-item-${cardId}] [testid=transfer-pause-btn]`, 8000);
      await huss_pc.uiClick(`[testid=transfer-item-${cardId}] [testid=transfer-pause-btn]`);
      pauseCaught = true;
      // 暂停语义:发送端停止服务新块,接收窗(4×4MiB)在途数据排空后才冻结
      // ——排空时长≈窗口字节/线速(不固定),故轮询至进度停走,再断言 2s 冻结
      const readCard = async () => {
        const t = (await huss_pc.state()).transfers.find((x) => x.id === cardId);
        if (!t) throw new Error('暂停目标卡消失');
        return t;
      };
      const t0 = await readCard();
      if (t0.state === 'done') throw new Error('暂停后卡片却 done（暂停未生效或过快）');
      const d0 = t0.bytesDone;
      let prev = d0, stable = 0, waited = 0;
      while (stable < 2 && waited < 20_000) {
        await sleep(700); waited += 700;
        const t = await readCard();
        if (t.state === 'done') throw new Error('暂停后卡片却 done（暂停未生效或过快）');
        if (t.bytesDone === prev) stable += 1; else { stable = 0; prev = t.bytesDone; }
      }
      if (prev - d0 > 24 * 1048576) throw new Error(`暂停后推进超过接收窗口: ${d0}→${prev}`);
      // 冻结断言:停走后 2s 内必须零推进(修复前暂停丢 FetchReq,恢复即死锁)
      await sleep(2000);
      const t3 = await readCard();
      if (t3.state === 'done') throw new Error('暂停排空后卡片却 done');
      if (t3.bytesDone !== prev) throw new Error(`暂停排空后仍在推进: ${prev}→${t3.bytesDone}`);
      await huss_pc.screenshot(join(runDir, 'v-06-huss_pc-paused.png'));
      // UI 继续（同卡）
      await huss_pc.uiWait(`[testid=transfer-item-${cardId}] [testid=transfer-resume-btn]`, 8000);
      await huss_pc.uiClick(`[testid=transfer-item-${cardId}] [testid=transfer-resume-btn]`);
    } catch (e) {
      if (/pause-btn/.test(e.message)) {
        const st = (await huss_pc.state()).transfers.find((t) => t.id === cardId);
        if (st?.state === 'done') note('传输过快，暂停窗口未捕获——跳过暂停/继续子步');
        else throw e;
      } else throw e;
    }

    // 同卡终态 + 字节核对（存在性断言，不管其他卡——多卡行为记疑点清单）
    await huss_pc.pollUntil(
      (s) => s.transfers.some((t) => t.id === cardId && t.state === 'done' && t.bytesDone === SIZE),
      { timeoutMs: 300_000, intervalMs: 1000, what: '目标卡 500MB done' },
    );
    await huss_laptop.pollUntil(
      (s) => s.transfers.some((t) => t.direction === 'pull' && t.bytesTotal === SIZE && t.state === 'done' && t.bytesDone === SIZE),
      { timeoutMs: 300_000, intervalMs: 1000, what: 'B 侧 500MB done' },
    );
    await huss_pc.screenshot(join(runDir, 'v-07-huss_pc-done.png'));
    await huss_laptop.screenshotSoft(join(runDir, 'v-08-huss_laptop-done.png'));
    return `500MB done${pauseCaught ? '（含暂停→停滞→继续全链路）' : '（暂停窗口过快跳过）'}，目标卡 ${cardId.slice(-4)}，A/B 字节一致`;
  });

  const big2 = makeBigFile(200 * 1048576);
  await step('D3 200MB 推送：UI 取消 → 终态 → UI 删卡 → 卡消失', async () => {
    const SIZE2 = statSync(big2).size;
    await huss_pc.invoke('push_files', { fingerprint: fpB, local_paths: [big2] });
    await huss_laptop.uiWait('.offer-modal', 20_000);
    await huss_laptop.uiClick('.offer-modal .btn-success');
    const hit = await huss_pc.pollUntil(
      (s) => s.transfers.find((t) => t.bytesTotal === SIZE2 && t.state === 'active' && t.bytesDone > 8 * 1048576)
        ?? false,
      { timeoutMs: 60_000, intervalMs: 300, what: '200MB 活动卡进度 >8MB' },
    );
    const cardId = hit.value.id;
    await huss_pc.uiNavigate('/transfers');
    await huss_pc.uiClick(`[testid=transfer-item-${cardId}] [testid=transfer-cancel-btn]`);
    // 取消后先经 cancelling 仲裁（非 active 也非终态）——必须等到真终态再操作
    const st = await huss_pc.pollUntil(
      (s) => {
        const t = s.transfers.find((x) => x.id === cardId);
        return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
      },
      { timeoutMs: 30_000, intervalMs: 500, what: '取消后目标卡达真终态(非cancelling)' },
    );
    const t = st.value;
    await huss_pc.screenshot(join(runDir, 'v-09-huss_pc-cancelled.png'));
    await huss_laptop.pollUntil((s) => !s.transfers.some((x) => x.speedBps > 0), { timeoutMs: 60_000, intervalMs: 1000, what: 'B 侧速度归零' });
    await huss_laptop.screenshotSoft(join(runDir, 'v-10-huss_laptop-after-cancel.png'));
    // UI 删卡：终态卡收进折叠的"历史"区（实测：主列表不渲染终态卡）——先展开再删
    await huss_pc.uiClick('[testid=transfers-history-fold-btn]');
    await huss_pc.uiWait(`[testid=transfer-item-${cardId}]`, 8000);
    await huss_pc.uiWait(`[testid=transfer-item-${cardId}] [testid=transfer-remove-btn]`, 12_000);
    await huss_pc.uiClick(`[testid=transfer-item-${cardId}] [testid=transfer-remove-btn]`);
    // 删除双分支：终态卡弹确认框（取消=仅列表移除，对应 removed:true）；
    // 非终态卡直删不弹窗。两种路径都收敛到"removed 标记或视图消失"
    const viaDialog = await huss_pc.uiWait('[testid=confirm-dialog]', 5000).then(() => true).catch(() => false);
    if (viaDialog) {
      await huss_pc.screenshot(join(runDir, 'v-09b-huss_pc-confirm-dialog.png'));
      await huss_pc.uiClick('[testid=confirm-cancel]');
    }
    // 删除语义：卡标记 removed=true（快照仍含历史卡）或从活动视图消失
    await huss_pc.pollUntil(
      (s) => !s.transfers.some((x) => x.id === cardId) || (s.cards || []).find((c) => c.cardId === cardId)?.removed === true,
      { timeoutMs: 15_000, what: '目标卡已删除(removed)' },
    );
    return `取消终态 state=${t.state}（${Math.round(t.bytesDone / 1048576)}/${Math.round(t.bytesTotal / 1048576)}MB），同卡删除后已从视图消失，B 侧已停止`;
  });

  // ---------- E Android 远程能力 ----------
  await step('E1 Android：唤醒 → banner → dump 见桌面设备 → tab 切换 → 截图', async () => {
    const ch = androidChannel();
    ch.setStayOn(true);
    let lastErr;
    // MIUI/adb 偶发瞬态（语义树空壳 / banner 丢失 / 息屏边缘态）靠整体重试吸收
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        return await e1Attempt(ch, attempt);
      } catch (e) {
        lastErr = e;
        note(`E1 第${attempt}次尝试失败: ${e.message.slice(0, 140)}`);
        await sleep(2000);
      }
    }
    throw lastErr;
  });

  /** E1 单次尝试（main 内函数声明，hoist 可用）
   * 关键实证：冷启动后 testTagsAsResourceId 的 resource-id 延迟 >90s 才发布（MIUI×Compose
   * 交互问题，已记遗留）——驱动热应用是稳定路径，forceStop 冷启仅留给显式重启场景 */
  async function e1Attempt(ch, attempt) {

    await sleep(1500);
    ch.launch(); // 热路径：已在运行则仅拉到前台
    if (attempt === 1) {
      // 首次尝试前确认应用在运行；未运行则冷启并给足语义发布时间
      const running = els => els.some((e) => e.package === ch.PACKAGE);
      let first = [];
      try { first = await ch.dump(); } catch { /* 屏幕未亮 */ }
      if (!running(first)) {
        await sleep(90_000); // 冷启动语义发布窗口
      }
    }
    // banner 降级为尽力而为：实测它与 UI 就绪相互独立地抖动，UI dump 才是就绪门槛
    const banner = await ch.waitForBanner(8_000).catch(() => null);
    let els = [];
    let navHit = 0;
    for (let i = 0; i < 8; i++) {
      els = await ch.dump();
      navHit = els.filter((e) => (e.testTag || '').includes('nav-')).length;
      note(`E1 attempt=${attempt} iter=${i} els=${els.length} navHit=${navHit} banner=${banner ? 'y' : 'n'}`);
      if (navHit >= 3) break;
      await sleep(1500);
    }
    if (navHit < 3) {
      let summary = '';
      try { summary = ch.dumpSummary(els, 12); } catch { /* 尽力 */ }
      throw new Error(`dump 仅 ${navHit} nav 元素(els=${els.length})；摘要 ${String(summary).slice(0, 120)}`);
    }
    await ch.tap({ testTag: 'nav-transfers-link' });
    await sleep(800);
    const after = await ch.dump();
    const sel = after.find((e) => (e.testTag || '') === 'nav-transfers-link');
    if (!sel?.selected) throw new Error('tab 切换后 selected 未置位');
    await ch.screenshot(join(runDir, 'v-11-android-transfers.png'));
    // 回设备页截图（应见桌面设备）
    await ch.tap({ testTag: 'nav-devices-link' });
    await sleep(800);
    await ch.screenshot(join(runDir, 'v-12-android-devices.png'));
    note(`banner: ${banner ? banner.slice(0, 120) : '(未捕获，UI 断言独立通过)'}`);
    return `dump ${els.length} 元素（nav×${navHit}），tab 切换 selected ✓${banner ? '，banner ✓' : '（banner 本轮未捕获）'}`;
  }

  // ---------- F 收尾 ----------
  await step('F1 证据收割 + test/end + 清场', async () => {
    const ev = await collectEvidence([huss_pc, huss_laptop], runDir, 'final');
    await huss_pc.endTest('pass');
    await huss_laptop.endTest('pass');
    await reset(1, [huss_pc, huss_laptop]);
    rmSync(join(e2eRoot(), 'fixtures', 'runs', runId), { recursive: true, force: true });
    return `证据 ${ev.reduce((s, e) => s + e.files.length, 0)} 件；大文件 fixture 已清理`;
  });
}

try {
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try { await collectEvidence([huss_pc, huss_laptop], runDir, 'failure'); } catch { /* 尽力 */ }
  // 先 endTest 再清场（重启会清服务端 run 上下文）
  try { await huss_pc.endTest('fail'); } catch { }
  try { await huss_laptop.endTest('fail'); } catch { }
  // 失败也清场：引擎残留会污染下一次运行（取消任务占活跃槽的疑点即由此暴露）
  try { await reset(2, [huss_pc, huss_laptop]); } catch (e2) { console.warn(`[reset] 失败清场未完成: ${e2.message}`); }
}

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '验收标准：远程完成一切操作/一切测试/收集一切日志，不打折扣',
      '功能断言 + 分步视觉证据（v-*.png）双轨；截图由验收人逐张复核渲染',
      'D2 暂停子步在传输过快时自动降级为跳过并注明',
      '接收确认弹窗按钮为 CSS 直选（offer 模态未铺 testid，已登记 M8 清理项）',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
