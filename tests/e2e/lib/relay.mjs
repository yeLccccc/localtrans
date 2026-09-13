// 中继服务器生命周期 + 双端端口基址隔离（T5 中继路径场景基建）。
//
// 拓扑决策（2026-09-07 实测约束，偏离计划卡"本机起 relay"的原因）：
// 1. 中继跑在 huss_laptop：其活动网卡为 Public 配置文件且防火墙关闭
//    （app get_network_status 实测 fw_public=false），入站免规则可达。
//    本机（huss_pc）三 profile 全启用、agent 会话非管理员无法 netsh 加规则，
//    且 target/release/localtrans-relay.exe 无任何按路径的入站放行规则
//    （现存 localtrans-relay.exe 规则指向旧 dist 路径且仅 Public profile）——
//    中继放本机则 huss_laptop 的控制/数据面入站必被防火墙拦死。
// 2. "直连不可达"隔离同样不经防火墙（无管理员权限），改用 T1 的
//    LOCALTRANS_TEST_PORT_BASE：双端不同基址（pc=50100 / laptop=50200）
//    → 监听与广播目标端口错开 → 本地发现互相不可达（行为级隔离，
//    无系统级防火墙副作用，恢复 = 按默认环境重启进程）。
//
// relay 配置字段名以 crates/localtrans-relay/src/config.rs::RelayConfig 为准：
// control_port / data_port_start / data_port_end / public_ip / psk(>=16) /
// lease_ttl_secs / auth_max_per_min。
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { isIP } from 'node:net';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { loadTargets, repoRoot } from './config.mjs';
import { sshExec, sftpUpload, stopPcB, stopPcA, waitReady } from './deploy.mjs';

/** 中继测试拓扑常量：端口避开常用段；PSK 32hex 测试值（与客户端一致） */
export const RELAY = {
  controlPort: 19443,
  dataStart: 19500,
  dataEnd: 19520,
  psk: '3f9a1c7e5b2d48069ad4c8f1e7b3a052',
};

/** 双端隔离端口基址（发现=base，QUIC=base+1；不同基址 = 本地发现互不可达） */
export const PORT_BASE = { huss_pc: 50100, huss_laptop: 50200 };

const REMOTE_RELAY_DIR_WIN = 'C:\\Users\\huss_laptop\\localtrans-test\\relay';
const REMOTE_APP_DIR_WIN = 'C:\\Users\\huss_laptop\\localtrans-test';
// sftp 用正斜杠，cmd（type/del/mkdir/schtasks /tr）必须反斜杠——
// cmd 把 `C:/x/y` 里的 `/y` 解析成开关（实测报"命令语法不正确"）
const REMOTE_RELAY_DIR = 'C:/Users/huss_laptop/localtrans-test/relay';
const REMOTE_APP_DIR = 'C:/Users/huss_laptop/localtrans-test';
const RELAY_TASK = 'LT-E2E-Relay';
const PB_TASK = 'LT-E2E-PB';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** relay 二进制：已有则用；缺失则 cargo build --release -p localtrans-relay */
export function relayBinary() {
  const p = join(repoRoot(), 'target', 'release', 'localtrans-relay.exe');
  if (!existsSync(p)) {
    const r = spawnSync('cargo', ['build', '--release', '-p', 'localtrans-relay'], {
      stdio: 'inherit', shell: true, cwd: repoRoot(), timeout: 10 * 60_000,
    });
    if (r.status !== 0 || !existsSync(p)) throw new Error('localtrans-relay 构建失败');
  }
  return p;
}

/** 最小 relay-test.toml（字段名 = RelayConfig；public_ip=名册/租约地址拼接用） */
export function relayToml(publicIp) {
  if (isIP(publicIp) === 0) throw new Error(`public_ip 不是 IP 字面量: ${publicIp}`);
  return [
    '# T5 中继路径场景专用配置（tests/e2e/lib/relay.mjs 生成，独立目录不污染部署）',
    `control_port = ${RELAY.controlPort}`,
    `data_port_start = ${RELAY.dataStart}`,
    `data_port_end = ${RELAY.dataEnd}`,
    `public_ip = "${publicIp}"`,
    `psk = "${RELAY.psk}"`,
    'lease_ttl_secs = 45',
    'auth_max_per_min = 5',
    '',
  ].join('\n');
}

/** 中继对外公布的 IP：huss_laptop 的 LAN IP（双端数据面都要可达） */
export function relayPublicIp(targets = loadTargets()) {
  const cand = targets.huss_laptop.ssh.host || targets.huss_laptop.host;
  if (isIP(cand) === 0) throw new Error(`huss_laptop 地址非 IP 字面量，无法作 public_ip: ${cand}`);
  return cand;
}

const relayLogPath = () => `${REMOTE_RELAY_DIR_WIN}\\relay-out.log`;

/** 读取中继 stdout 日志（RUST_LOG=debug，含 Punch/KNOCK 证据行） */
export async function relayLog(targets = loadTargets()) {
  const r = await sshExec(targets.huss_laptop, `type ${relayLogPath()}`, { ignoreCode: true });
  return r.out;
}

/** 等中继就绪：日志出现启动行（控制面+数据面绑定完成） */
export async function waitRelayReady({ timeoutMs = 20_000 } = {}, targets = loadTargets()) {
  const deadline = Date.now() + timeoutMs;
  let log = '';
  while (Date.now() < deadline) {
    log = await relayLog(targets);
    if (log.includes('中继启动') && log.includes('psk 摘要')) {
      return log;
    }
    await sleep(500);
  }
  throw new Error(`中继 ${timeoutMs}ms 内未就绪，日志尾:\n${log.slice(-800)}`);
}

/** 部署并启动中继（huss_laptop，schtasks 交互任务；幂等：先杀旧进程删旧任务） */
export async function startRelay(targets = loadTargets()) {
  const publicIp = relayPublicIp(targets);
  const bin = relayBinary();
  // 幂等清场：旧进程/旧日志/旧任务
  await sshExec(targets.huss_laptop, 'taskkill /F /IM localtrans-relay.exe', { ignoreCode: true });
  await sshExec(targets.huss_laptop, `schtasks /delete /tn ${RELAY_TASK} /f`, { ignoreCode: true });
  await sshExec(targets.huss_laptop, `if not exist "${REMOTE_RELAY_DIR_WIN}" mkdir "${REMOTE_RELAY_DIR_WIN}"`);
  await sshExec(targets.huss_laptop, `del /q "${relayLogPath()}"`, { ignoreCode: true });

  await sftpUpload(targets.huss_laptop, bin, `${REMOTE_RELAY_DIR}/localtrans-relay.exe`);
  const toml = join(tmpdir(), `lt-relay-test-${process.pid}.toml`);
  writeFileSync(toml, relayToml(publicIp));
  await sftpUpload(targets.huss_laptop, toml, `${REMOTE_RELAY_DIR}/relay-test.toml`);
  // 启动 bat：debug 日志落文件（Punch/KNOCK 行 = 走中继的进程级证据）
  const bat = [
    '@echo off',
    `cd /d ${REMOTE_RELAY_DIR_WIN}`,
    'set RUST_LOG=debug',
    'localtrans-relay.exe relay-test.toml > relay-out.log 2>&1',
  ].join('\r\n') + '\r\n';
  const batLocal = join(tmpdir(), `lt-relay-start-${process.pid}.bat`);
  writeFileSync(batLocal, bat);
  await sftpUpload(targets.huss_laptop, batLocal, `${REMOTE_RELAY_DIR}/start-relay.bat`);

  await sshExec(
    targets.huss_laptop,
    `schtasks /create /tn ${RELAY_TASK} /tr "${REMOTE_RELAY_DIR}\\start-relay.bat" /sc once /st 23:59 /it /f`,
  );
  // schtasks /IT + 电池供电时"不用电池启动"——laptop 用电池时任务静默不跑
  // (同 M6 LT-Test 坑;实测电源条件还导致 /run 报成功但不执行)。创建后立即
  // 用 PowerShell 修电源条件再启动。relay 纯后台服务,无 GUI 会话诉求。
  await sshExec(targets.huss_laptop,
    `powershell -NoProfile -Command "Set-ScheduledTask -TaskName ${RELAY_TASK} -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries); Start-ScheduledTask -TaskName ${RELAY_TASK}"`);
  const log = await waitRelayReady({}, targets);
  return { publicIp, controlPort: RELAY.controlPort, logBrief: log.split('\n').filter((l) => l.includes('中继启动')).join(' | ') };
}

/** 停中继 + 删计划任务（进程残留一并清） */
export async function stopRelay(targets = loadTargets()) {
  const errs = [];
  try { await sshExec(targets.huss_laptop, 'taskkill /F /IM localtrans-relay.exe', { ignoreCode: true }); }
  catch (e) { errs.push(`taskkill: ${e.message}`); }
  try { await sshExec(targets.huss_laptop, `schtasks /delete /tn ${RELAY_TASK} /f`, { ignoreCode: true }); }
  catch (e) { errs.push(`schtasks delete: ${e.message}`); }
  if (errs.length) throw new Error(errs.join('; '));
  return 'relay 进程已停、任务已删';
}

// ---------------------------------------------------------------------------
// 双端端口基址隔离
// ---------------------------------------------------------------------------

/** 确保 huss_laptop 端口基址启动 bat + 计划任务就位（bat 与 start-test.bat 同目录，test-api.key 走 %~dp0） */
export async function ensureLaptopPortBaseTask(targets = loadTargets()) {
  const bat = [
    '@echo off',
    `rem T5 中继场景隔离专用：与 start-test.bat 同 env，另设 LOCALTRANS_TEST_PORT_BASE=${PORT_BASE.huss_laptop}`,
    'set LOCALTRANS_TEST_API=1',
    'set /p LOCALTRANS_TEST_API_KEY=<"%~dp0test-api.key"',
    `set LOCALTRANS_TEST_API_PORT=${targets.huss_laptop.port}`,
    `set LOCALTRANS_TEST_PORT_BASE=${PORT_BASE.huss_laptop}`,
    'cd /d "%~dp0"',
    'start "" localtrans.exe',
  ].join('\r\n') + '\r\n';
  const local = join(tmpdir(), `lt-start-test-pb-${process.pid}.bat`);
  writeFileSync(local, bat);
  await sftpUpload(targets.huss_laptop, local, `${REMOTE_APP_DIR}/start-test-pb.bat`);
  await sshExec(
    targets.huss_laptop,
    `schtasks /create /tn ${PB_TASK} /tr "${REMOTE_APP_DIR_WIN}\\start-test-pb.bat" /sc once /st 23:59 /it /f`,
  );
  return PB_TASK;
}

/** 清理隔离任务（不杀进程——进程由调用方按需 L2/杀） */
export async function removeLaptopTasks(targets = loadTargets()) {
  const errs = [];
  for (const tn of [PB_TASK]) {
    try { await sshExec(targets.huss_laptop, `schtasks /delete /tn ${tn} /f`, { ignoreCode: true }); }
    catch (e) { errs.push(`${tn}: ${e.message}`); }
  }
  if (errs.length) throw new Error(errs.join('; '));
}

/**
 * 以指定端口基址在 huss_pc 本机启动 app（Start-Process 脱离会话，env 子进程继承）。
 * 与 deploy.startPcA 同款流程 + LOCALTRANS_TEST_PORT_BASE（不改动 deploy.mjs，
 * 该文件有并行任务在改；恢复默认基址直接用 reset(2)/deploy.startPcA）。
 */
export function startPcWithPortBase(portBase, targets = loadTargets()) {
  const exe = join(repoRoot(), 'target', 'release', 'localtrans.exe');
  if (!existsSync(exe)) throw new Error(`产物不存在: ${exe}（先跑 deploy 构建）`);
  console.log(`[relay] 启动 huss_pc（LOCALTRANS_TEST_PORT_BASE=${portBase}）…`);
  const ps = [
    `$env:LOCALTRANS_TEST_API='1'`,
    `$env:LOCALTRANS_TEST_API_KEY='${targets.huss_pc.token}'`,
    `$env:LOCALTRANS_TEST_API_PORT='${targets.huss_pc.port}'`,
    `$env:LOCALTRANS_TEST_PORT_BASE='${portBase}'`,
    `Start-Process -FilePath '${exe}'`,
  ].join('; ');
  const r = spawnSync('powershell', ['-NoProfile', '-Command', ps], { shell: true, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`huss_pc 端口基址启动失败: ${r.stderr || r.stdout}`);
}

// ---------------------------------------------------------------------------
// config.json 端口补丁：壳层绑定端口读持久化 config（data/config.json，
// exe 同目录），env 默认值仅在字段缺失时生效——隔离必须改文件。
// 远端用 PowerShell 正则替换（无 BOM UTF8 读写，保留其余字段）；本机 node 直改。
// ---------------------------------------------------------------------------
const PC_CONFIG = join(repoRoot(), 'target', 'release', 'data', 'config.json');
const LAPTOP_CONFIG = `${REMOTE_APP_DIR_WIN}\\data\\config.json`;
const PATCH_PS1_REMOTE = `${REMOTE_APP_DIR_WIN}\\patch-ports.ps1`;

const PATCH_PS1 = [
  'param([string]$Path, [int]$Discovery, [int]$Quic)',
  '$enc = New-Object System.Text.UTF8Encoding($false)',
  '$c = [System.IO.File]::ReadAllText($Path)',
  "$c = $c -replace '\"discovery_port\"\\s*:\\s*\\d+', ('\"discovery_port\": ' + $Discovery)",
  "$c = $c -replace '\"quic_port\"\\s*:\\s*\\d+', ('\"quic_port\": ' + $Quic)",
  '[System.IO.File]::WriteAllText($Path, $c, $enc)',
  '$j = [System.IO.File]::ReadAllText($Path) | ConvertFrom-Json',
  'Write-Output "$($j.discovery_port) $($j.quic_port)"',
].join('\r\n') + '\r\n';

/** 读双端 config.json 当前端口（记录原值，恢复用）；输出 {discovery_port, quic_port} */
export async function readAppPorts(targets = loadTargets()) {
  const pcCfg = JSON.parse(readFileSync(PC_CONFIG, 'utf8'));
  const r = await sshExec(
    targets.huss_laptop,
    `powershell -NoProfile -Command "$j=[System.IO.File]::ReadAllText('${LAPTOP_CONFIG}') | ConvertFrom-Json; Write-Output \\"$($j.discovery_port) $($j.quic_port)\\""`,
  );
  const [d, q] = r.out.trim().split(/\s+/).map(Number);
  if (!d || !q) throw new Error(`huss_laptop config.json 端口读取失败: ${r.out}`);
  return {
    pc: { discovery_port: pcCfg.discovery_port, quic_port: pcCfg.quic_port },
    huss_laptop: { discovery_port: d, quic_port: q },
  };
}

/** 双端 config.json 端口补丁为指定基址；返回各端 PowerShell/node 回读值供断言 */
async function writeAppPorts(pcBase, laptopBase, targets = loadTargets()) {
  // pc：node 直改（JSON 语义等价回写）
  const pcCfg = JSON.parse(readFileSync(PC_CONFIG, 'utf8'));
  pcCfg.discovery_port = pcBase;
  pcCfg.quic_port = pcBase + 1;
  writeFileSync(PC_CONFIG, JSON.stringify(pcCfg, null, 2) + '\n');
  // laptop：上传 ps1（幂等）→ 执行 → 回读校验
  const ps1Local = join(tmpdir(), `lt-patch-ports-${process.pid}.ps1`);
  writeFileSync(ps1Local, PATCH_PS1);
  await sftpUpload(targets.huss_laptop, ps1Local, `${REMOTE_APP_DIR}/patch-ports.ps1`);
  const r = await sshExec(
    targets.huss_laptop,
    `powershell -NoProfile -ExecutionPolicy Bypass -File "${PATCH_PS1_REMOTE}" -Path "${LAPTOP_CONFIG}" -Discovery ${laptopBase} -Quic ${laptopBase + 1}`,
  );
  const [d, q] = r.out.trim().split(/\s+/).map(Number);
  if (d !== laptopBase || q !== laptopBase + 1) {
    throw new Error(`huss_laptop config.json 端口补丁失败: 期望 ${laptopBase}/${laptopBase + 1}，回读 ${r.out}`);
  }
  return `pc→${pcBase}/${pcBase + 1}，laptop→${laptopBase}/${laptopBase + 1}`;
}

/**
 * 双端以各自隔离基址重启（杀默认实例 → 补丁 config.json 端口 → 带 env 拉起 → 等 bridgeReady）。
 * @returns 原端口值（restoreAppPorts 恢复用）
 */
export async function restartIsolated(targets = loadTargets()) {
  stopPcA();
  await stopPcB(targets);
  const saved = await readAppPorts(targets);
  const patched = await writeAppPorts(PORT_BASE.huss_pc, PORT_BASE.huss_laptop, targets);
  await ensureLaptopPortBaseTask(targets);
  // 同 relay 任务:电池供电下 schtasks /IT 静默不跑——先修电源条件再启动
  await sshExec(targets.huss_laptop,
    `powershell -NoProfile -Command "Set-ScheduledTask -TaskName ${PB_TASK} -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries); Start-ScheduledTask -TaskName ${PB_TASK}"`);
  startPcWithPortBase(PORT_BASE.huss_pc, targets);
  await waitReady(targets.huss_laptop, { label: 'huss_laptop@PB' });
  await waitReady(targets.huss_pc, { label: 'huss_pc@PB' });
  return { saved, patched };
}

/** 恢复双端 config.json 原端口（清理路径，先于 L2 回默认调用） */
export async function restoreAppPorts(saved, targets = loadTargets()) {
  return writeAppPorts(saved.pc.discovery_port, saved.huss_laptop.discovery_port, targets);
}
