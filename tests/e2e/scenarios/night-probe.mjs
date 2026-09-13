// 夜批探针（N1-T1/T2）：单文件推送发送端卡片数取证 + 取消后再推是否卡 pending。
// 只观测不修：dump 全量卡片（含 removed），每步打印到控制台，供定位。
import { mkdirSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Target } from '../lib/target.mjs';
import { reset } from '../lib/reset.mjs';

const targets = loadTargets();
const A = new Target('huss_pc', targets.huss_pc, null);
const B = new Target('huss_laptop', targets.huss_laptop, null);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const MB = 1024 * 1024;

const runId = `probe-${Date.now().toString(36)}`;
const fxDir = join(e2eRoot(), 'fixtures', 'runs', runId);
mkdirSync(fxDir, { recursive: true });

/** 唯一内容文件（头部 64B 内含完整 runId 戳，其后 0x5a 填充）——
 *  头部区与填充区隔离，保证两次运行内容必不同（避开接收端秒传干扰观测） */
function probeFile(name, sizeMb) {
  const p = join(fxDir, name);
  const buf = Buffer.alloc(sizeMb * MB);
  const header = Buffer.from(`LT-PROBE ${runId} ${name}\n`, 'utf8');
  header.copy(buf, 0);
  buf.fill(0x5a, 64);
  writeFileSync(p, buf);
  return p;
}

/** dump 双端活动视图 probe 卡 + 全卡表计数 */
async function dumpCards(tag) {
  for (const [label, t] of [['A(发送端)', A], ['B(接收端)', B]]) {
    const s = await t.state();
    const act = (s.transfers || []).filter((x) => (x.name || '').startsWith('probe-'));
    console.log(`[${tag}] ${label} 活动probe卡=${act.length} (全卡表${(s.cards || []).length})`);
    for (const x of act) console.log(`   ${JSON.stringify(x)}`);
  }
}

function assert(cond, msg) { if (!cond) throw new Error(msg); }

/** B 端真实点"接收"（push=Ask）；无弹窗则如实报告（auto/instant 场景） */
async function acceptOnB(tag) {
  try {
    await B.uiWait('.offer-modal .btn-success', 30_000);
    await B.uiClick('.offer-modal .btn-success');
    console.log(`[${tag}] B 已点接收`);
  } catch {
    const s = await B.state();
    console.log(`[${tag}] B 30s 未见接收弹窗（transfers=${(s.transfers || []).length}）`);
  }
}

try {
  console.log('== 准备：双端健康 + L2 重启清僵尸 + L1 清卡 + 连接 ==');
  for (const t of [A, B]) assert((await t.health()).bridgeReady, `${t.name} bridgeReady`);
  await reset(2, [A, B]);
  await reset(1, [A, B]);
  const fpB = (await B.state()).selfDevice.id ?? (await B.stateApp()).selfDevice?.fingerprint;
  console.log('fpB =', fpB);
  await A.invoke('connect', { fingerprint: fpB });
  await A.pollUntil((s) => (s.sessions ?? []).length > 0, { timeoutMs: 20_000, what: '会话建立' });

  const f1 = probeFile('probe-1mb.bin', 1);
  const f16 = probeFile('probe-16mb.bin', 16);
  const f200 = probeFile('probe-200mb.bin', 200);

  console.log('\n== P1: 单文件小推送(1MB) ==');
  {
    const card = await A.invoke('push_files', { fingerprint: fpB, local_paths: [f1] });
    console.log('push_files 返回占位 cardId =', card);
    await acceptOnB('P1');
    await A.pollUntil((s) => (s.transfers || []).some((t) => (t.bytesTotal ?? t.total) === 1 * MB
      && ['done', 'failed', 'interrupted'].includes(t.state)), { timeoutMs: 60_000, what: 'P1 终态' });
    await sleep(1500); // 等尾事件落完
    await dumpCards('P1');
  }

  console.log('\n== P2: 单文件大推送(16MB，可能走接收方回拉路径) ==');
  {
    const card = await A.invoke('push_files', { fingerprint: fpB, local_paths: [f16] });
    console.log('push_files 返回占位 cardId =', card);
    await acceptOnB('P2');
    await A.pollUntil((s) => (s.transfers || []).some((t) => (t.bytesTotal ?? t.total) === 16 * MB
      && ['done', 'failed', 'interrupted'].includes(t.state)), { timeoutMs: 90_000, what: 'P2 终态' });
    await sleep(1500);
    await dumpCards('P2');
  }

  console.log('\n== P3(T2): 200MB 推送→UI 取消→再推 1MB 观察是否卡 pending ==');
  {
    const card = await A.invoke('push_files', { fingerprint: fpB, local_paths: [f200] });
    console.log('push_files 返回占位 cardId =', card);
    await acceptOnB('P3');
    const hit = await A.pollUntil((s) => (s.transfers || []).find((t) =>
      (t.bytesTotal ?? t.total) === 200 * MB && t.state === 'active'
      && (t.bytesDone ?? t.done) > 8 * MB), { timeoutMs: 60_000, what: 'P3 活动>8MB' });
    const cid = hit.value.id ?? hit.value.card_id ?? card;
    await A.uiNavigate('/transfers');
    await A.uiClick(`[testid=transfer-item-${cid}] [testid=transfer-cancel-btn]`);
    await A.pollUntil((s) => {
      const t = (s.transfers || []).find((x) => (x.id ?? x.card_id) === cid || (x.bytesTotal ?? x.total) === 200 * MB);
      return t && ['done', 'failed', 'interrupted'].includes(t.state) ? t : false;
    }, { timeoutMs: 30_000, what: 'P3 取消终态' });
    await sleep(1500);
    await dumpCards('P3-取消后');

    // 取消后立刻再推 1MB：观测是否长期 pending（T2 症状——按名匹配，占位卡 total=0）
    const t0 = Date.now();
    const card2 = await A.invoke('push_files', { fingerprint: fpB, local_paths: [f1] });
    console.log('取消后再推 push_files 返回 =', card2);
    await acceptOnB('P3-再推');
    let sawActive = false; let lastSeen = null;
    while (Date.now() - t0 < 45_000) {
      const s = await A.state();
      const t = (s.transfers || []).find((x) => x.id === card2);
      if (t) {
        lastSeen = t;
        if (t.state !== 'pending') { sawActive = true; break; }
      }
      await sleep(1500);
    }
    console.log(sawActive
      ? `T2 复检 ✓：再推成功离开 pending（${JSON.stringify(lastSeen)}）`
      : `T2 症状：再推 45s 仍 pending（${JSON.stringify(lastSeen)}）`);
    await dumpCards('P3-再推后');
  }

  console.log('\n探针完成（观测数据见上，不自动清卡——保留现场）');
} finally {
  rmSync(fxDir, { recursive: true, force: true });
  if (!existsSync(fxDir)) console.log('(probe fixtures 已清理)');
}
