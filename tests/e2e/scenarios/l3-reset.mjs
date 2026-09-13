// 场景：l3-reset（T2，spec §7.4.5）——L3 出厂重置全链路验收 + 夹具自愈
//
// 流程：双端握手并记录重置前状态（指纹/信任/配置）→ 停双端 → 备份 data/ 六文件
// （huss_pc 本地拷贝；huss_laptop sftp 拉回）→ reset(3) 出厂重置双端 → 断言如新装
// （信任空 / 默认共享区 / transfers 空 / 指纹已换新）→ reset(3,{keepIdentity,seedTrust})
// 播种恢复演练 → 断言信任表还原且真实建会话（connect → 双端 sessions trusted）→
// 夹具自愈：回灌备份（身份/配置/信任全量）+ L2 重启 → 断言与重置前逐项一致。
//
// 任何一步失败：collectEvidence 后报告标 FAIL，退出码 1，并尽力回灌备份恢复夹具。
// 用法：node scenarios/l3-reset.mjs   （前置：node lib/deploy.mjs 已部署双端）
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { loadTargets, e2eRoot } from '../lib/config.mjs';
import { Journal } from '../lib/journal.mjs';
import { Target } from '../lib/target.mjs';
import { collectEvidence } from '../lib/evidence.mjs';
import { writeReport } from '../lib/report.mjs';
import { reset, restoreDataFiles, L3_DATA_FILES, PCB_DATA_DIR, pcDataDir } from '../lib/reset.mjs';
import { sshExec, sftpDownload, stopPcA, stopPcB } from '../lib/deploy.mjs';

const SCENARIO = 'l3-reset';
const ts = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const rand = Math.random().toString(36).slice(2, 6);
const runId = `${SCENARIO}-${ts}-${rand}`;
const runDir = join(e2eRoot(), 'reports', runId);
mkdirSync(runDir, { recursive: true });

const journal = new Journal(runDir);
const targets = loadTargets();
const huss_pc = new Target('huss_pc', targets.huss_pc, journal);
const huss_laptop = new Target('huss_laptop', targets.huss_laptop, journal);
const both = [huss_pc, huss_laptop];

const steps = [];
const startedAt = new Date();
const versions = {};
let failureMsg = null;
const pre = {}; // 重置前状态快照 {name: {fp, deviceName, trustedFps}}
const backup = {}; // {name: {dir, files: [...]}}
const PCB_WIN_DIR = PCB_DATA_DIR.split('/').join('\\');

const F = (n) => `reports/${runId}/${n}`;
const note = (m) => journal.append({ kind: 'note', message: m });

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

const fingerprintOf = async (t) => (await t.invoke('get_device_fingerprint')).fingerprint_hex;

async function remoteFileExists(winPath) {
  const { out } = await sshExec(targets.huss_laptop, `if exist "${winPath}" (echo __YES__) else (echo __NO__)`, { ignoreCode: true });
  return out.includes('__YES__');
}

/** 备份单端 data/：返回实际备份的文件名数组（identity/config 缺失视为夹具损坏，调用方断言） */
async function backupData(t) {
  const dir = join(runDir, 'fixture-backup', t.name);
  mkdirSync(dir, { recursive: true });
  const copied = [];
  for (const f of L3_DATA_FILES) {
    if (t.name === 'huss_pc') {
      const src = join(pcDataDir(t.name), f);
      if (!existsSync(src)) continue;
      copyFileSync(src, join(dir, f));
    } else {
      if (!(await remoteFileExists(`${PCB_WIN_DIR}\\${f}`))) continue;
      await sftpDownload(targets.huss_laptop, `${PCB_DATA_DIR}/${f}`, join(dir, f));
    }
    copied.push(f);
  }
  backup[t.name] = { dir, files: copied };
  return copied;
}

/** 信任表文件（JSON）读取（备份副本） */
function backupTrust(name) {
  const p = join(backup[name].dir, 'trusted_peers.json');
  if (!existsSync(p)) return [];
  return JSON.parse(readFileSync(p, 'utf8'));
}

/** 夹具自愈：回灌备份 + L2 重启（restoreDataFiles 内部含停进程）。 */
async function restoreFixture() {
  await restoreDataFiles(both.map((t) => ({ name: t.name, from: backup[t.name].dir, files: backup[t.name].files })));
}

/** 收尾 test/end：本场景多轮重启双端，run 无法跨进程存续——
 * 结束前重新 begin（给后端一条带最终结论的完整 run 记录），失败仅记笔记不上抛 */
async function endTestSafe(t, outcome) {
  try {
    await t.beginTest(SCENARIO);
    await t.endTest(outcome);
  } catch (e) { note(`${t.name} test/end 失败（多轮重启 run 不连续，编排器报告为准）: ${e.message}`); }
}

async function main() {
  console.log(`[${SCENARIO}] 开始  runId=${runId}`);

  await step('环境握手 + 记录重置前状态', async () => {
    for (const t of both) {
      const v = await t.version();
      if (v.bridgeReady !== true) throw new Error(`${t.name} bridge 未就绪`);
      versions[t.name] = v;
      const fpDto = await t.invoke('get_device_fingerprint');
      const trusted = await t.invoke('list_trusted');
      const settings = await t.invoke('get_settings');
      pre[t.name] = {
        fp: fpDto.fingerprint_hex,
        deviceName: settings.device_name,
        trustedFps: trusted.map((p) => p.fingerprint).sort(),
      };
    }
    if (!/^[0-9a-f]{64}$/.test(pre.huss_pc.fp)) throw new Error(`huss_pc 指纹非 64hex: ${pre.huss_pc.fp}`);
    note(`重置前指纹 huss_pc=${pre.huss_pc.fp} huss_laptop=${pre.huss_laptop.fp}`);
    note(`重置前信任 huss_pc=${pre.huss_pc.trustedFps.length} 条，huss_laptop=${pre.huss_laptop.trustedFps.length} 条`);
    return `huss_pc v${versions.huss_pc.appVersion} fp=${pre.huss_pc.fp.slice(0, 8)}…，huss_laptop v${versions.huss_laptop.appVersion} fp=${pre.huss_laptop.fp.slice(0, 8)}…`;
  });

  await step('停双端 + 备份 data/（夹具自愈凭据）', async () => {
    // 静默态备份（进程停止后再拷，规避 transfers.json 写一半）
    stopPcA();
    await stopPcB(targets);
    for (const t of both) {
      const files = await backupData(t);
      for (const must of ['identity.key', 'cert.der', 'config.json']) {
        if (!files.includes(must)) throw new Error(`${t.name} 备份缺 ${must}——夹具不可自愈，拒绝继续`);
      }
      note(`${t.name} 备份 ${files.length} 文件 → ${backup[t.name].dir}`);
    }
    return `huss_pc ${backup.huss_pc.files.length} 文件、huss_laptop ${backup.huss_laptop.files.length} 文件落 ${F('fixture-backup/…')}`;
  });

  await step('L3 出厂重置双端（含身份→新指纹）', async () => {
    return await reset(3, both);
  });

  await step('断言：双端如新装（信任空/默认共享区/新指纹）', async () => {
    const detail = [];
    for (const t of both) {
      const trusted = await t.invoke('list_trusted');
      if (!Array.isArray(trusted) || trusted.length !== 0) {
        throw new Error(`${t.name} 信任表非空: ${JSON.stringify(trusted)}`);
      }
      const settings = await t.invoke('get_settings');
      if (!Array.isArray(settings.shares) || settings.shares.length === 0) {
        throw new Error(`${t.name} 无默认共享区: ${JSON.stringify(settings.shares)}`);
      }
      const share = settings.shares[0];
      if (share.alias !== '共享文件夹' || !/\\share$/.test(share.path)) {
        throw new Error(`${t.name} 共享区非出厂默认: ${JSON.stringify(share)}`);
      }
      const snap = await t.state();
      // L3 只清 data/（transfers.json）；downloads/ 残留 .part 的断点续传卡
      // （state=interrupted）重启后由续传引擎重建，属磁盘残留而非清空失败——
      // 只断言不得出现上一轮生命周期的历史卡（done 等）
      const history = snap.transfers.filter((x) => x.state !== 'interrupted');
      if (history.length !== 0) {
        throw new Error(`${t.name} transfers.json 未清（历史卡）: ${JSON.stringify(history.map((x) => x.name))}`);
      }
      if (snap.transfers.length > 0) {
        note(`${t.name} 有 ${snap.transfers.length} 张 downloads 残留断点续传卡（interrupted，非 data 清理失败）: ${snap.transfers.map((x) => x.name).join(',')}`);
      }
      const fp = await fingerprintOf(t);
      if (!/^[0-9a-f]{64}$/.test(fp)) throw new Error(`${t.name} 新指纹非 64hex: ${fp}`);
      if (fp === pre[t.name].fp) throw new Error(`${t.name} 指纹未变——identity.key 未被清掉`);
      t.newFp = fp;
      detail.push(`${t.name}: 信任0/共享「${share.alias}」/fp ${fp.slice(0, 8)}…（原 ${pre[t.name].fp.slice(0, 8)}…）`);
    }
    // fresh_config 的 device_name=COMPUTERNAME：本机可精确断言，远端主机名未知不苛求
    const pcSettings = await huss_pc.invoke('get_settings');
    if (pcSettings.device_name !== process.env.COMPUTERNAME) {
      throw new Error(`huss_pc 出厂名应=COMPUTERNAME(${process.env.COMPUTERNAME})，实际 ${pcSettings.device_name}`);
    }
    detail.push(`huss_pc 出厂名=${pcSettings.device_name}`);
    return detail.join('；');
  });

  await step('seedTrust 播种恢复（keepIdentity 保指纹）', async () => {
    // 互播对方当前指纹（keepIdentity 下两台身份均未再变，条目真实有效）
    return await reset(3, both, {
      keepIdentity: true,
      seedTrust: {
        huss_pc: [{ fingerprint: huss_laptop.newFp, name: 'huss_laptop' }],
        huss_laptop: [{ fingerprint: huss_pc.newFp, name: 'huss_pc' }],
      },
    });
  });

  await step('断言：信任表恢复 + connect 真实建会话', async () => {
    for (const t of both) {
      const trusted = await t.invoke('list_trusted');
      if (trusted.length !== 1) throw new Error(`${t.name} 信任表应 1 条，实际 ${trusted.length}`);
      const want = t === huss_pc ? huss_laptop.newFp : huss_pc.newFp;
      if (trusted[0].fingerprint !== want) throw new Error(`${t.name} 信任指纹不符: ${trusted[0].fingerprint}`);
      if (trusted[0].push !== 'ask') throw new Error(`${t.name} 播种 perms.push 应 ask: ${trusted[0].push}`);
    }
    // 等双方发现「对方本尊」（devices.length 会把手机也算进去，不得作数），
    // 再 connect（信任命中路径，免配对码）
    await Promise.all([
      huss_pc.pollUntil((s) => s.devices.some((d) => d.id === huss_laptop.newFp),
        { timeoutMs: 30_000, what: 'huss_pc 发现 huss_laptop 新指纹' }),
      huss_laptop.pollUntil((s) => s.devices.some((d) => d.id === huss_pc.newFp),
        { timeoutMs: 30_000, what: 'huss_laptop 发现 huss_pc 新指纹' }),
    ]);
    // 对端刚重启，connect 偶发早于会话通道就绪——短重试
    let lastConnErr = null;
    for (let i = 0; i < 3; i++) {
      try { await huss_pc.invoke('connect', { fingerprint: huss_laptop.newFp }); lastConnErr = null; break; }
      catch (e) { lastConnErr = e; await new Promise((r) => setTimeout(r, 3000)); }
    }
    if (lastConnErr) throw lastConnErr;
    await Promise.all([
      huss_pc.pollUntil((s) => s.sessions.some((x) => x.peer === huss_laptop.newFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_pc sessions 含新 HUSS 指纹且 trusted' }),
      huss_laptop.pollUntil((s) => s.sessions.some((x) => x.peer === huss_pc.newFp && x.trusted),
        { timeoutMs: 30_000, what: 'huss_laptop sessions 含新 huss_pc 指纹且 trusted' }),
    ]);
    return `双向信任恢复，sessions trusted=true（fp ${huss_pc.newFp.slice(0, 8)}…↔${huss_laptop.newFp.slice(0, 8)}…）`;
  });

  await step('夹具自愈：回灌备份 + 重启', async () => {
    await restoreFixture();
    return `回灌 huss_pc ${backup.huss_pc.files.length} + huss_laptop ${backup.huss_laptop.files.length} 文件，双端已重启就绪`;
  });

  await step('断言：夹具与重置前一致', async () => {
    const detail = [];
    for (const t of both) {
      const fp = await fingerprintOf(t);
      if (fp !== pre[t.name].fp) throw new Error(`${t.name} 指纹未还原: ${fp} != ${pre[t.name].fp}`);
      const settings = await t.invoke('get_settings');
      if (settings.device_name !== pre[t.name].deviceName) {
        throw new Error(`${t.name} device_name 未还原: ${settings.device_name} != ${pre[t.name].deviceName}`);
      }
      const trustedFps = (await t.invoke('list_trusted')).map((p) => p.fingerprint).sort();
      if (JSON.stringify(trustedFps) !== JSON.stringify(pre[t.name].trustedFps)) {
        throw new Error(`${t.name} 信任表未还原: [${trustedFps}] != [${pre[t.name].trustedFps}]`);
      }
      detail.push(`${t.name}: fp/名/信任${trustedFps.length}条 一致`);
    }
    return detail.join('；');
  });

  await step('收尾：证据 + test/end', async () => {
    const arts = [];
    for (const t of both) {
      // 截图是证据非断言：huss_laptop 显示器离位时 /api/screenshot 500（环境问题，
      // 见报告备注），不因此判场景失败
      try {
        await t.screenshot(join(runDir, `${t.name}-final.png`));
        arts.push(F(`${t.name}-final.png`));
      } catch (e) { note(`${t.name} 收尾截图失败（证据缺口，非断言）: ${e.message}`); }
    }
    const ev = await collectEvidence(both, runDir, 'final');
    for (const e of ev) arts.push(...e.files.map(F));
    await endTestSafe(huss_pc, 'pass');
    await endTestSafe(huss_laptop, 'pass');
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
  try { await huss_pc.endTest('fail'); } catch { /* 尽力 */ }
  try { await huss_laptop.endTest('fail'); } catch { /* 尽力 */ }
  // 夹具自愈兜底：失败也要把备份灌回去（否则双端停在出厂态，phone/其他场景夹具尽毁）
  if (backup.huss_pc?.files?.length && backup.huss_laptop?.files?.length) {
    console.log(`[${SCENARIO}] 失败兜底：回灌备份恢复夹具…`);
    try { await restoreFixture(); note('失败兜底夹具回灌完成'); }
    catch (e3) { note(`失败兜底夹具回灌失败: ${e3.message}`); console.error(`[fixture] 回灌失败: ${e3.message}`); }
  }
}

const outcome = failureMsg ? 'fail' : 'pass';
const finishedAt = new Date();
const reportFile = writeReport({
  runId, scenario: SCENARIO, steps, outcome, failure: failureMsg || undefined,
  env: {
    runDir, startedAt: startedAt.toISOString(), finishedAt: finishedAt.toISOString(),
    durationMs: finishedAt - startedAt, versions,
    notes: [
      '信任断言走 invoke list_trusted——trustedPeers 不在 get_settings(ConfigDto) 字段中（信任/配置分属两个只读命令）',
      '出厂断言：信任空 + shares=[共享文件夹@<exe旁>/share] + transfers 空 + 指纹更换（fresh_config device_name=COMPUTERNAME，本机侧精确断言）',
      'seedTrust 演练配 keepIdentity（保指纹）——出厂新身份播种旧指纹只验证写表机制，真实会话须播当前指纹',
      '夹具自愈为全量回灌（身份/配置/信任/传输记录），huss_pc 信任含「我的手机」条目一并还原；手机侧出厂重置为 adb.mjs factoryReset()（pm clear+MIUI 弹窗放行），不在本 PC 场景执行以免毁手机夹具',
      '已知环境问题：huss_laptop /api/screenshot 500「未定位到窗口所在显示器」（显示器离位），9/6 报告尚正常，与 L3 无关——收尾截图按证据尽力而为处理',
      '场景自身多次重启双端（L3×2 + 夹具回灌），全程约 2-4 分钟属预期',
    ],
  },
});
const pass = steps.filter((s) => s.status === 'PASS').length;
console.log(`\n[${SCENARIO}] ${pass}/${steps.length} 步骤通过  →  ${outcome.toUpperCase()}`);
console.log(`[${SCENARIO}] 报告: ${reportFile}`);
process.exit(outcome === 'pass' ? 0 : 1);
