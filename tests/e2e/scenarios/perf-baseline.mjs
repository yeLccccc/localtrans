// 场景：perf-baseline（P4 性能卡 T1 大文件吞吐 + T2 小文件批量，2026-09-09）
//
// ★ 口径：PC→手机 **WiFi 实测**（真实用户主路径）。手机 USB 线只是 adb 控制面，
//   传输本身走 QUIC over WiFi。huss_laptop 环回对照为可选段（可达且已配对才跑，
//   1×500MB）；中继口径留双机遗留（relay_status 只读观测，不部署中继，不可用即
//   条件跳过记录）。手机不可达 → 整场条件跳过（exit 0，run-all 不连坐）。
//
// T1：PC 推 1GB 单文件给手机 ×3 取中位：
//   - 发送端卡字节速率：100ms 粒度 /api/state/transfers 采样（speedBps + bytesDone）
//   - 全程 MB/s：字节数 ÷（对端接收确认→终态）秒表（1 MB = 1 MiB = 1048576 B）
//   - PC 进程 CPU%：CIM PercentProcessorTime（单核口径，1s 尽力采样，失败=null 不致死）
//   - 结束内存：Win32_Process WorkingSetSize
//   断言：3 轮全部 done 且 bytesDone == 1GiB（1073741824 字节核对）
// T2：200×10KB push_files 单 offer 批量——总耗时+速率记录；断言父卡 done。
//
// 证据：reports/perf-baseline-<ts>-<rand>/ 下 result.json + 速率曲线 JSON + 截图；
//   docs/audit/perf-baseline.md 由 lib/perfreport.mjs 统一再生（含 T3/T4 最新工件）。
// 前置：node lib/deploy.mjs --skip-huss_laptop 已部署 PC 端；
//   android/app/build/outputs/apk/debug/app-debug.apk 已 assemble。
import { mkdirSync, writeFileSync, openSync, writeSync, ftruncateSync, closeSync, rmSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { loadTargets, e2eRoot, repoRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset } from '../lib/reset.mjs';
import { resolveAdbPath } from '../lib/adb.mjs';
import { stopPcA, startPcA, waitReady } from '../lib/deploy.mjs';
import { regeneratePerfReport } from '../lib/perfreport.mjs';
import { sampleProcessCpu, processWorkingSetBytes } from '../lib/winmetrics.mjs';

const SCENARIO = 'perf-baseline';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, journal);
const laptop = new Target('huss_laptop', targets.huss_laptop, journal);

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
let skipReason = null;

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const GIB = 1073741824; // 1 GiB
const MB = 1048576;
const APK = join(repoRoot(), 'android', 'app', 'build', 'outputs', 'apk', 'debug', 'app-debug.apk');
const PHONE_RECV_DIR = '/sdcard/Download/LocalTrans';

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

/** 可达探测（短超时；targets 配置是 host/port 平面结构） */
const baseOf = (cfg) => `http://${cfg.host}:${cfg.port}`;
const reachable = (base) => fetch(base + '/api/health', { signal: AbortSignal.timeout(4000) }).then((r) => r.ok).catch(() => false);

/** 手机 adb 可用性探测（设备离线/未插时 get-state 非零退出） */
function phoneAvailable() {
  try {
    const adb = resolveAdbPath();
    const serial = targets.huss_phone?.serial;
    if (!serial) return false;
    execFileSync(adb, ['-s', serial, 'get-state'], { timeout: 5000, windowsHide: true, stdio: 'pipe' });
    return true;
  } catch { return false; }
}

// ---- 条件跳过判定（先于任何步骤；口径=PC→手机 WiFi，手机是硬前置）----
const pcUp = await reachable(baseOf(targets.huss_pc));
const phoneUp = pcUp ? phoneAvailable() : false;
const lapUp = pcUp ? await reachable(baseOf(targets.huss_laptop)) : false;
if (!pcUp) skipReason = 'huss_pc test-api 不可达（先跑 node lib/deploy.mjs --skip-huss_laptop）';
else if (!phoneUp) skipReason = 'huss_phone 不可达——吞吐基线口径=PC→手机 WiFi,手机是硬前置（条件跳过）';
if (skipReason) {
  console.log(`[${SCENARIO}] SKIP: ${skipReason}`);
  const reportFile = writeReport({
    runId, scenario: SCENARIO, steps: [], outcome: 'skip',
    env: { runDir, startedAt: startedAt.toISOString(), finishedAt: new Date().toISOString(), notes: [skipReason] },
  });
  console.log(`[${SCENARIO}] 报告: ${reportFile}`);
  process.exit(0);
}

const ch = (await import('../lib/adb.mjs')).default(targets.huss_phone.adb);
const adbRun = (args, opts = {}) => execFileSync(resolveAdbPath(), ['-s', targets.huss_phone.serial, ...args],
  { encoding: 'utf8', timeout: 20000, windowsHide: true, ...opts });
/** 手机接收文件清理（尽力，失败不阻断——存储清理不是断言对象） */
const rmPhoneFile = (name) => { try { adbRun(['shell', 'rm', '-f', `${PHONE_RECV_DIR}/${name}`]); } catch { /* 尽力 */ } };

/** fixture（头部 runId 戳 + 截断,规避秒传去重;跑完即删） */
function makeFixture(name, sizeBytes) {
  const dir = join(e2eRoot(), 'fixtures', 'runs', runId);
  mkdirSync(dir, { recursive: true });
  const p = join(dir, name);
  const fd = openSync(p, 'w');
  writeSync(fd, Buffer.from(`perf-baseline-stamp:${runId}:${name}`));
  ftruncateSync(fd, sizeBytes);
  closeSync(fd);
  return p;
}

/** 发送端 100ms 粒度速率采样器：state speedBps + bytesDone 序列 */
function startRateSampler(getCard) {
  const series = [];
  let stopped = false;
  const done = (async () => {
    while (!stopped) {
      const t = Date.now();
      try {
        const c = await getCard();
        series.push({ t, done: c.bytesDone, total: c.bytesTotal, speedBps: c.speedBps });
      } catch (e) { series.push({ t, error: String(e.message || e).slice(0, 120) }); }
      const dt = 100 - (Date.now() - t);
      if (dt > 0) await sleep(dt);
    }
  })();
  return { series, stop: () => { stopped = true; }, done };
}

/** PC 进程 CPU 采样器（1s 尽力采样；CIM 慢时自然稀疏） */
function startCpuSampler() {
  const series = [];
  let stopped = false;
  const done = (async () => {
    while (!stopped) {
      const t = Date.now();
      const cpu = await sampleProcessCpu();
      series.push({ t, cpuPercent: cpu });
      const dt = 1000 - (Date.now() - t);
      if (dt > 0) await sleep(dt);
    }
  })();
  return { series, stop: () => { stopped = true; }, done };
}

/** 手机 OfferSheet 点接收：testid 优先,回落文本节点中心点按（Compose a11y 双节点问题） */
async function phoneAcceptOffer({ timeoutMs = 25_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    let btn = null;
    try {
      const offerEls = await ch.dump();
      btn = offerEls.find((e) => e.testTag === 'offer-accept-btn')
        || offerEls.find((e) => /接收|接受|允许/.test((e.text || '') + (e.contentDesc || '')));
    } catch { /* dump 抖动重试 */ }
    if (btn) {
      await ch.tapXY(btn.center[0], btn.center[1]);
      return true;
    }
    await sleep(800);
  }
  return false;
}

// ---- 主流程状态 ----
const run = { fpPeer: null };
const result = { runs: [], batch: null, laptopLoopback: null, relayNote: null, observations: [], finishedAt: null };

async function pushOnce({ fixturePath, fileName, runLabel }) {
  // 启动采样器 → push_files → 手机点接收 → 等终态 → 断言字节。返回计时与采样。
  const cardId = await A.invoke('push_files', { fingerprint: run.fpPeer, local_paths: [fixturePath] });
  await A.uiNavigate('/transfers');
  await A.uiWait('[testid^=transfer-item-]', 15_000);

  const rate = startRateSampler(async () => {
    const s = await A.state();
    const card = s.transfers.find((x) => x.id === cardId);
    if (!card) throw new Error('目标卡消失');
    return card;
  });
  const cpu = startCpuSampler();
  const acceptedAt = Date.now();
  const accepted = await phoneAcceptOffer();
  if (!accepted) { rate.stop(); cpu.stop(); throw new Error(`${runLabel}: 手机应出现接收弹窗`); }
  const acceptDoneAt = Date.now();

  const hit = await A.pollUntil((s) => {
    const t = s.transfers.find((x) => x.id === cardId);
    return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
  }, { timeoutMs: 600_000, intervalMs: 1000, what: `${runLabel} 推送终态` });
  const terminalAt = Date.now();
  rate.stop(); cpu.stop();
  await rate.done; await cpu.done;
  if (hit.value.state !== 'done') throw new Error(`${runLabel}: 推送应 done,实际 ${hit.value.state}:${hit.value.failReason || ''}`);
  return { cardId, acceptedAt, acceptDoneAt, terminalAt, card: hit.value, rateSeries: rate.series, cpuSeries: cpu.series };
}

async function main() {
  await step('环境握手：huss_pc health/version + 手机可达', async () => {
    const v = await A.version();
    if (v.bridgeReady !== true) throw new Error('huss_pc bridge 未就绪');
    versions.huss_pc = v;
    return `huss_pc v${v.appVersion}(${v.buildProfile}),手机 serial=${targets.huss_phone.serial}`;
  });

  await step('清场：PC 重启（单机 L2,不依赖 huss_laptop）+ 手机重装/亮屏', async () => {
    stopPcA();
    startPcA(loadTargets());
    await waitReady(loadTargets().huss_pc, { label: 'huss_pc' });
    ch.wake(); ch.setStayOn(true); await sleep(1200);
    await ch.install(APK);
    ch.clearLogcat(); ch.launch();
    const banner = await ch.waitForBanner(30_000);
    note('banner: ' + banner.slice(0, 60));
    await sleep(2500);
    // 配对自动同意钩子（设置页 debug 区,滚动找;auto_consent_pairing 只影响配对,
    // 推送 offer 仍走真实点接收）
    await ch.tap({ testTag: 'nav-settings-link' }); await sleep(1200);
    let hook = null;
    for (let i = 0; i < 4 && !hook; i++) {
      const els = await ch.dump();
      hook = els.find((e) => e.testTag === 'settings-test-auto-consent-toggle');
      if (!hook) { ch.swipe(540, 1750, 540, 750); await sleep(700); }
    }
    if (hook && !hook.checked) { await ch.tap({ testTag: 'settings-test-auto-consent-toggle' }); await sleep(600); }
    await ch.tap({ testTag: 'nav-devices-link' }); await sleep(1000);
    return 'PC 单机重启就绪 + 手机 APK 重装/横幅/配对钩子 ✓';
  });

  await step('发现/配对（幂等,手机发起）+ test/begin', async () => {
    await A.beginTest(SCENARIO);
    const s0 = await A.state();
    const phone = (s0.devices || []).find((d) => /huss_phone|我的手机/.test(d.name || '') && d.online);
    if (!phone) throw new Error('huss_pc 未在线见到 huss_phone: ' + JSON.stringify((s0.devices || []).map((d) => d.name)));
    run.fpPeer = phone.id;
    const fpPc = s0.selfDevice.id;
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
      if (!code) throw new Error('PC 应显示 6 位配对码');
      await sleep(800);
      els = await ch.dump();
      const input = els.find((e) => e.testTag === 'code-input');
      if (!input) throw new Error('手机应有输码框(code-input)');
      await ch.tap({ testTag: 'code-input' });
      await ch.inputText(code);
      await ch.tap({ testTag: 'btn-submit' });
    }
    await A.pollUntil((s) => (s.sessions ?? []).some((x) => x.peer === run.fpPeer),
      { timeoutMs: 30_000, intervalMs: 1000, what: '会话建立' });
    return `对端指纹 ${run.fpPeer.slice(0, 8)}…,会话就绪`;
  });

  await step('T1 吞吐基线：1GB 单文件推手机 ×3（100ms 速率采样 + CPU + 内存）', async () => {
    const arts = [];
    for (const i of [1, 2, 3]) {
      const fileName = `perf1g-${rand}-r${i}.bin`;
      const fixture = makeFixture(fileName, GIB);
      let x;
      try {
        x = await pushOnce({ fixturePath: fixture, fileName, runLabel: `T1-r${i}` });
      } finally {
        try { rmSync(fixture, { force: true }); } catch { /* 跑完即删,尽力 */ }
      }
      const SIZE = x.card.bytesTotal;
      if (x.card.bytesDone !== GIB) throw new Error(`T1-r${i}: 字节核对失败 ${x.card.bytesDone}/${GIB}`);
      if (SIZE !== GIB) note(`T1-r${i}: 卡 total=${SIZE}（与 fixture 1GiB 不一致,按卡值记录）`);
      const durationMs = x.terminalAt - x.acceptedAt; // 全程口径:对端接收确认→终态
      const mbps = GIB / (durationMs / 1000) / MB;
      const cpuVals = x.cpuSeries.map((s) => s.cpuPercent).filter((v) => v !== null);
      const cpuAvg = cpuVals.length ? cpuVals.reduce((a, b) => a + b, 0) / cpuVals.length : null;
      const mem = await processWorkingSetBytes();
      // 首字节窗口（首样 done>0）——启动爬升观测,参考值
      const firstByte = x.rateSeries.find((s) => s.done > 0);
      const shot = join(runDir, `v-pc-t1-r${i}-final.png`);
      await A.screenshotSoft(shot);
      if (existsSync(shot)) arts.push(F(`v-pc-t1-r${i}-final.png`));
      result.runs.push({
        run: i, name: fileName, sizeBytes: GIB, durationMs, mbps,
        cpuAvgPercent: cpuAvg === null ? null : Number(cpuAvg.toFixed(2)),
        cpuSamples: cpuVals.length,
        memWorkingSetBytes: mem,
        acceptLagMs: x.acceptDoneAt - x.acceptedAt,
        firstByteLagMs: firstByte ? firstByte.t - x.acceptedAt : null,
      });
      writeFileSync(join(runDir, `rate-r${i}.json`), JSON.stringify(
        { run: i, cardId: x.cardId, sizeBytes: GIB, acceptedAt: x.acceptedAt, terminalAt: x.terminalAt, samples: x.rateSeries }, null, 2));
      arts.push(F(`rate-r${i}.json`));
      // 观测:发送端 speedBps 显示口径 vs 秒表端到端口径（done 按 QUIC 突发推进,EMA 偏乐观）
      const burst = x.rateSeries.filter((s) => s.done > 0 && s.speedBps > 0).map((s) => s.speedBps).sort((a, b) => a - b);
      if (burst.length > 10) {
        const medBps = burst[Math.floor(burst.length / 2)];
        result.observations.push(
          `T1-r${i}: 发送端 speedBps 显示口径中位 ${(medBps / MB).toFixed(1)} MB/s,秒表端到端 ${mbps.toFixed(1)} MB/s` +
          `（发送端 done 按 ~8MB QUIC 突发推进,EMA 显示口径偏乐观——基线以秒表口径为准,显示口径偏差留打磨池核查）`);
      }
      console.log(`    T1-r${i}: ${mbps.toFixed(1)} MB/s (${(durationMs / 1000).toFixed(1)}s) CPU ${cpuAvg === null ? '—' : cpuAvg.toFixed(1) + '%'} mem ${mem === null ? '—' : (mem / MB).toFixed(0) + 'MB'}`);
      // 轮间软清终态卡,下轮干净起步（数据保留）
      await reset(1, [A]).catch((e) => note(`轮间 L1 清理失败(不致死): ${e.message}`));
      rmPhoneFile(fileName);
    }
    const byMbps = [...result.runs].sort((a, b) => a.mbps - b.mbps);
    const med = byMbps[1];
    result.median = { ...med };
    return `3 轮 done=1GiB 核对通过;中位 ${med.mbps.toFixed(1)} MB/s（各轮 ${result.runs.map((r) => r.mbps.toFixed(1)).join('/')}）`;
  });

  await step('T2 小文件批量：200×10KB push_files 单 offer（总耗时+速率）', async () => {
    const stamp = rand;
    const dir = join(runDir, `batch-${stamp}`);
    mkdirSync(dir, { recursive: true });
    const names = [];
    for (let i = 0; i < 200; i++) {
      const n = `p200-${stamp}-${String(i).padStart(4, '0')}.bin`;
      const b = Buffer.alloc(10 * 1024); b.write(`p200 ${stamp} ${i}`, 0, 'utf8'); b.fill(0x55, 32);
      writeFileSync(join(dir, n), b);
      names.push(join(dir, n));
    }
    const cardId = await A.invoke('push_files', { fingerprint: run.fpPeer, local_paths: names });
    await A.uiNavigate('/transfers');
    await A.uiWait('[testid^=transfer-item-]', 15_000);
    const acceptedAt = Date.now();
    const accepted = await phoneAcceptOffer();
    if (!accepted) throw new Error('T2: 手机应出现接收弹窗');
    const hit = await A.pollUntil((s) => {
      const t = s.transfers.find((x) => x.id === cardId);
      return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
    }, { timeoutMs: 180_000, intervalMs: 500, what: 'T2 批量父卡终态' });
    if (hit.value.state !== 'done') throw new Error(`T2: 批量父卡应 done,实际 ${hit.value.state}:${hit.value.failReason || ''}`);
    const totalMs = Date.now() - acceptedAt;
    const bytesTotal = hit.value.bytesTotal;
    result.batch = {
      files: 200, fileBytes: 10 * 1024, bytesTotal, totalMs,
      mbps: Number((bytesTotal / (totalMs / 1000) / MB).toFixed(2)),
      state: hit.value.state,
    };
    const shot = join(runDir, 'v-pc-t2-final.png');
    await A.screenshotSoft(shot);
    const shotP = join(runDir, 'v-phone-t2-final.png');
    try { ch.screenshot(shotP); } catch { /* 尽力 */ }
    // 收尾：本机批量 fixture 目录 + 手机 200 个接收文件
    try { rmSync(dir, { recursive: true, force: true }); } catch { /* 尽力 */ }
    try { adbRun(['shell', 'rm', '-f', `${PHONE_RECV_DIR}/p200-${stamp}-*.bin`]); } catch { /* 尽力 */ }
    return `200 文件/${(bytesTotal / MB).toFixed(2)}MB 父卡 done,耗时 ${(totalMs / 1000).toFixed(2)}s,${result.batch.mbps} MB/s`;
  });

  await step('可选对照：huss_laptop 环回 500MB（可达且已配对才跑,失败不致死）', async () => {
    if (!lapUp) return '条件跳过: huss_laptop 不可达（主口径不受影响=PC→手机 WiFi）';
    try {
      const s = await A.state();
      const b = s.devices.find((d) => d.name === 'huss_laptop');
      if (!b) return '条件跳过: 未发现 huss_laptop';
      try { await A.invoke('connect', { fingerprint: b.id }); } catch { await sleep(3000); await A.invoke('connect', { fingerprint: b.id }); }
      await A.pollUntil((s2) => s2.sessions.some((x) => x.peer === b.id && x.trusted), { timeoutMs: 15_000, intervalMs: 1000, what: 'laptop 会话 trusted' })
        .catch(() => { throw new Error('laptop 会话未 trusted（历史信任缺失）——对照段条件跳过'); });
      const fileName = `perf-loopback-${rand}.bin`;
      const fixture = makeFixture(fileName, 500 * MB);
      try {
        await laptop.dismissResumePrompt();
        const loopCardId = await A.invoke('push_files', { fingerprint: b.id, local_paths: [fixture] });
        await laptop.uiWait('.offer-modal', 20_000).catch(() => { throw new Error('laptop offer 弹窗未出现'); });
        await laptop.uiClick('.offer-modal .btn-success');
        const t0 = Date.now();
        const hit = await A.pollUntil((s2) => {
          const t = s2.transfers.find((x) => x.id === loopCardId);
          return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
        }, { timeoutMs: 300_000, intervalMs: 1000, what: '环回对照终态' });
        const totalMs = Date.now() - t0;
        if (hit.value.state !== 'done') throw new Error(`对照推送 ${hit.value.state}`);
        result.laptopLoopback = { sizeBytes: 500 * MB, totalMs, mbps: Number((500 * MB / (totalMs / 1000) / MB).toFixed(1)) };
        await reset(1, [A, laptop]).catch(() => { });
        return `环回对照 1×500MB: ${result.laptopLoopback.mbps} MB/s（PC-PC LAN,非主口径）`;
      } finally {
        try { rmSync(fixture, { force: true }); } catch { /* 跑完即删 */ }
      }
    } catch (e) {
      note(`环回对照段失败(不致死): ${e.message}`);
      return `对照段未完成(不致死): ${e.message.slice(0, 120)}`;
    }
  });

  await step('中继口径观测（只读,不部署中继——不可用即记录遗留）', async () => {
    try {
      const rs = await A.invoke('relay_status', {});
      const enabled = rs && (rs.enabled ?? rs.configured);
      result.relayNote = `relay_status=${JSON.stringify(rs).slice(0, 160)} → 中继吞吐口径留双机遗留${enabled ? '' : '（中继未启用）'}`;
    } catch (e) {
      result.relayNote = `relay_status 查询失败(${String(e.message).slice(0, 60)}) → 中继吞吐口径留双机遗留`;
    }
    note(result.relayNote);
    return result.relayNote;
  });

  await step('基线报告再生 + 证据落盘 + test/end', async () => {
    const arts = [];
    result.finishedAt = new Date().toISOString();
    writeFileSync(join(runDir, 'result.json'), JSON.stringify(result, null, 2));
    arts.push(F('result.json'));
    const doc = regeneratePerfReport('perf-baseline', result);
    console.log(`  基线报告: ${doc}`);
    const ev = await collectEvidence([A], runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    await A.endTest('pass');
    return { detail: `证据 ${arts.length} 件;报告 docs/audit/perf-baseline.md 已再生`, artifacts: arts };
  });
}

try {
  console.log(`[${SCENARIO}] 开始  runId=${runId}  对端=huss_phone(WiFi 主口径)  laptop对照=${lapUp ? '可达' : '跳过'}`);
  await main();
} catch (e) {
  failureMsg = e.message;
  console.error(`\n[${SCENARIO}] 失败: ${e.message}`);
  note(`FAIL: ${e.message}\n${e.stack || ''}`);
  try {
    const ev = await collectEvidence([A], runDir, `failure-${steps.findLast((s) => s.status === 'FAIL')?.name || 'unknown'}`);
    void ev;
  } catch (e2) { console.warn(`[evidence] 收割失败: ${e2.message}`); }
  try { await A.endTest('fail'); } catch { /* 尽力 */ }
}

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '口径:PC→手机 WiFi 实测(真实用户主路径);全程 MB/s = 1GiB ÷(对端接收确认→终态)秒表;1MB=1MiB',
      'CPU% 为单核口径(CIM PercentProcessorTime,100%=1 核),1s 尽力采样,失败=null 不致死',
      '中继口径条件跳过(留双机遗留);huss_laptop 环回对照可达才跑',
      'fixture(1GB×3+500MB)与手机接收文件跑完即删;文档基线由 lib/perfreport.mjs 再生',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
