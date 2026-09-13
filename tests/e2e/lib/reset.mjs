// 三级重置（spec §7.4.5）：
//   L1 软重置：invoke clear_completed_transfers（已完成卡片、toast 视图级清理，数据保留）
//   L2 应用重启：双端杀进程 + 带 env 重启（确认 pid 已死再启，规避 single-instance
//      只聚焦旧实例的坑）——复用 deploy.mjs 的启停（huss_pc 本地 Start-Process / huss_laptop schtasks）
//   L3 出厂重置：停进程 → data/ 六文件清空（config/信任/传输记录/探针目标/身份，
//      保留 logs/）→ 可选信任播种 → 重启 → 轮询 health。
//      手机侧出厂重置见 adb.mjs 的 factoryReset()（pm clear + MIUI 弹窗放行）。
//      opts.keepIdentity=true 跳过 identity.key/cert.der 删除——夹具恢复时指纹不变，
//      播种的信任条目才指向仍然有效的身份；opts.seedTrust 重启前写回
//      trusted_peers.json（数组=所有目标同表；对象=按目标名分表，如
//      {huss_pc:[{fingerprint,name}], huss_laptop:[...]}），条目格式即 store.rs
//      TrustedPeer 的 JSON 形态，缺省字段自动补全。
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { loadTargets, repoRoot } from './config.mjs';
import { startPcA, stopPcA, deployPcB, stopPcB, waitReady, sshExec, sftpUpload } from './deploy.mjs';

const log = (m) => console.log(`[reset] ${m}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** L3 清空的 data/ 文件清单（identity.* 两件受 opts.keepIdentity 保护）
 * connect_memory.json：M3a FR5 连接记忆——出厂重置语义下记忆一并清零 */
export const L3_DATA_FILES = [
  'config.json', 'trusted_peers.json', 'transfers.json',
  'probe_targets.json', 'connect_memory.json', 'identity.key', 'cert.der',
];
const IDENTITY_FILES = ['identity.key', 'cert.der'];

/** huss_laptop 数据目录（exe 旁，与 deploy.mjs 的 PCB_DIR 同基） */
export const PCB_DATA_DIR = 'C:/Users/huss_laptop/localtrans-test/data';

/** PC 端 data 目录（exe 旁）：huss_pc=本机 target/release/data，huss_laptop=远端固定路径。
 * 兼做目标合法性校验（非 PC 目标抛错，手机走 adb.mjs factoryReset）。 */
export function pcDataDir(name) {
  if (name === 'huss_pc') return join(repoRoot(), 'target', 'release', 'data');
  if (name === 'huss_laptop') return PCB_DATA_DIR;
  throw new Error(`L3 仅支持 huss_pc/huss_laptop，收到 ${name}（手机走 adb factoryReset）`);
}

/**
 * @param {1|2|3} level
 * @param {import('./target.mjs').Target[]} targets 双端 Target 实例
 * @param {object} [opts] L1/L2: {redeploy: huss_laptop 重启是否重推 exe（默认否，仅杀+任务启动）}
 *   L3: {keepIdentity: 不删身份文件（指纹不变，夹具恢复用）,
 *        seedTrust: 信任播种，数组=所有目标同表 / 对象=按目标名分表}
 */
export async function reset(level, targets, opts = {}) {
  if (level === 1) {
    for (const t of targets) {
      await t.invoke('clear_completed_transfers');
    }
    return 'L1 完成：双端终态卡片已 view 级清理（transfers.json 保留）';
  }
  if (level === 2) {
    const cfg = loadTargets();
    stopPcA();
    await stopPcB(cfg);
    if (opts.redeploy) {
      await deployPcB(cfg); // 杀 + scp + schtasks
    } else {
      const { sshExec } = await import('./deploy.mjs');
      await sshExec(cfg.huss_laptop, 'schtasks /run /tn LT-Test');
    }
    startPcA(cfg);
    await waitReady(cfg.huss_laptop, { label: 'huss_laptop' });
    await waitReady(cfg.huss_pc, { label: 'huss_pc' });
    // L2 重建 Target 连接语义：runId 失效（新进程无活动 run），调用方重新 begin
    for (const t of targets) t.runId = null;
    return 'L2 完成：双端进程重启且 bridgeReady=true';
  }
  if (level === 3) {
    const cfg = loadTargets();
    const keepIdentity = opts.keepIdentity === true;
    const seeds = normalizeSeedTrust(opts.seedTrust, targets.map((t) => t.name));
    const files = keepIdentity ? L3_DATA_FILES.filter((f) => !IDENTITY_FILES.includes(f)) : L3_DATA_FILES;
    // 按传入顺序逐台处理（停→清→播种→启→就绪）；需要交叉播种时由调用方
    // 保证 seedTrust 指向 keepIdentity 保护下的稳定指纹
    for (const t of targets) {
      const name = t.name;
      const dir = pcDataDir(name); // 非法目标在此抛错
      log(`${name}：停进程…`);
      if (name === 'huss_pc') stopPcA();
      else await stopPcB(cfg);

      log(`${name}：清空 data/ ${files.length} 文件${keepIdentity ? '（保留身份，指纹不变）' : '（含身份→新指纹）'}…`);
      if (name === 'huss_pc') wipeLocalData(dir, files);
      else await wipeRemoteData(cfg, dir, files);

      if (seeds[name]?.length) {
        log(`${name}：播种信任表 ${seeds[name].length} 条…`);
        await writeTrustFile(name, cfg, seeds[name]);
      }

      if (name === 'huss_pc') startPcA(cfg);
      else await sshExec(cfg.huss_laptop, 'schtasks /run /tn LT-Test');
      await waitReady(cfg[name], { label: name });
      t.runId = null; // 进程已换，run 失效（与 L2 同语义），调用方重新 begin
    }
    return `L3 完成：${targets.map((t) => t.name).join('+')} 出厂重置`
      + `${keepIdentity ? '（保留身份）' : '（新身份）'}${opts.seedTrust ? '+信任播种' : ''}，bridgeReady=true`;
  }
  throw new Error(`未知重置级别: ${level}（1=软清 2=重启 3=出厂）`);
}

// ---------------------------------------------------------------------------
// L3 内部：本地/远端清理 与 信任播种
// ---------------------------------------------------------------------------

/** 本机 data 文件清理（force+重试：taskkill 后句柄释放可能滞后） */
function wipeLocalData(dir, files) {
  for (const f of files) {
    const p = join(dir, f);
    rmSync(p, { force: true, maxRetries: 8, retryDelay: 300 });
    if (existsSync(p)) throw new Error(`huss_pc data 清理失败（句柄未释放？）: ${p}`);
  }
}

/** 远端目录列表；目录不存在（首跑前）返回 null */
async function remoteListDir(cfg, winDir) {
  const { out } = await sshExec(
    cfg.huss_laptop,
    `if exist "${winDir}" (dir /b "${winDir}") else (echo __NO_DIR__)`,
    { ignoreCode: true },
  );
  if (out.includes('__NO_DIR__')) return null;
  return out.split('\n').map((l) => l.trim()).filter(Boolean);
}

/** huss_laptop data 文件清理（ssh del；杀进程后句柄释放可能滞后，带校验重试） */
async function wipeRemoteData(cfg, dir, files) {
  const winDir = dir.split('/').join('\\');
  for (let attempt = 1; attempt <= 8; attempt++) {
    const listing = await remoteListDir(cfg, winDir);
    if (listing === null) return; // 目录尚不存在=干净
    const left = files.filter((f) => listing.some((l) => l.toLowerCase() === f.toLowerCase()));
    if (left.length === 0) return;
    if (attempt > 1) await sleep(500);
    for (const f of left) {
      await sshExec(cfg.huss_laptop, `del /f /q "${winDir}\\${f}"`, { ignoreCode: true });
    }
  }
  const listing = (await remoteListDir(cfg, winDir)) || [];
  const left = files.filter((f) => listing.some((l) => l.toLowerCase() === f.toLowerCase()));
  throw new Error(`huss_laptop data 清理失败，重试后仍残留: ${left.join(', ')}`);
}

/** 播种条目规范化：fingerprint 必须 64hex、name 必填，其余缺省补全（store.rs TrustedPeer 形态） */
function normalizeSeedEntries(entries, what) {
  if (!Array.isArray(entries)) throw new Error(`seedTrust.${what} 必须为数组`);
  return entries.map((e) => {
    if (!e || !/^[0-9a-fA-F]{64}$/.test(e.fingerprint || '')) {
      throw new Error(`seedTrust.${what} 条目 fingerprint 非法（须 64hex）: ${e?.fingerprint}`);
    }
    if (!e.name) throw new Error(`seedTrust.${what} 条目缺 name（fingerprint=${e.fingerprint?.slice(0, 8)}…）`);
    return {
      fingerprint: e.fingerprint.toLowerCase(),
      name: e.name,
      alias: e.alias || '',
      paired_at: e.paired_at ?? Math.floor(Date.now() / 1000),
      perms: e.perms ?? { browse: true, download: true, push: 'ask' },
    };
  });
}

/** seedTrust 归一：数组→所有目标同表；对象→按目标名分表（键必须都在本次目标内） */
function normalizeSeedTrust(seedTrust, names) {
  if (!seedTrust) return {};
  const out = {};
  if (Array.isArray(seedTrust)) {
    const entries = normalizeSeedEntries(seedTrust, '*');
    for (const n of names) out[n] = entries;
    return out;
  }
  for (const k of Object.keys(seedTrust)) {
    if (!names.includes(k)) throw new Error(`seedTrust 键 ${k} 不在本次重置目标 [${names.join(',')}] 中`);
    out[k] = normalizeSeedEntries(seedTrust[k], k);
  }
  return out;
}

/** 重启前写回 trusted_peers.json：huss_pc 本地写；huss_laptop 经 sftp 推 */
async function writeTrustFile(name, cfg, entries) {
  const json = JSON.stringify(entries, null, 2);
  if (name === 'huss_pc') {
    writeFileSync(join(pcDataDir(name), 'trusted_peers.json'), json, 'utf8');
    return;
  }
  const tmpDir = mkdtempSync(join(tmpdir(), 'lt-seed-'));
  const tmp = join(tmpDir, 'trusted_peers.json');
  writeFileSync(tmp, json, 'utf8');
  try {
    await sftpUpload(cfg.huss_laptop, tmp, `${PCB_DATA_DIR}/trusted_peers.json`);
  } finally {
    rmSync(tmpDir, { recursive: true, force: true });
  }
}

/** 夹具恢复便捷封装（l3-reset 场景回灌备份用）：停进程→逐文件回灌→L2 重启。
 * files: [{name: 'huss_pc'|'huss_laptop', from: 备份目录, files: 文件名数组}] */
export async function restoreDataFiles(files) {
  const cfg = loadTargets();
  stopPcA();
  await stopPcB(cfg);
  for (const spec of files) {
    const dir = pcDataDir(spec.name); // 校验 + 取路径
    for (const f of spec.files) {
      const src = join(spec.from, f);
      if (!existsSync(src)) throw new Error(`备份缺失: ${src}`);
      if (spec.name === 'huss_pc') {
        copyFileSync(src, join(dir, f));
      } else {
        await sftpUpload(cfg.huss_laptop, src, `${PCB_DATA_DIR}/${f}`);
      }
    }
  }
  return reset(2, []);
}
