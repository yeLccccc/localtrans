// 一次性引导：用密码（从环境变量 LT_SSH_PW 传入，不落盘）连接测试机，
// 做环境体检 + 安装公钥。之后全部走密钥认证（tests/e2e/ssh/id_ed25519）。
// 目标主机/用户名属本机拓扑,一律经环境变量传入,不写入仓库:
//   LT_SSH_PW='...' LT_SSH_HOST='<ip>' LT_SSH_USER='<user>' node lib/sshSetup.mjs
//   可选 LT_SSH_PING_TARGET='<ip>' 启用"回 ping 开发机"体检项
import { Client } from 'ssh2';
import { readFileSync } from 'node:fs';

const HOST = process.env.LT_SSH_HOST;
const USER = process.env.LT_SSH_USER;
const PUBKEY = readFileSync(new URL('../ssh/id_ed25519.pub', import.meta.url), 'utf8').trim();
const PW = process.env.LT_SSH_PW;
if (!PW || !HOST || !USER) {
  console.error('缺少环境变量:LT_SSH_PW / LT_SSH_HOST / LT_SSH_USER(本机拓扑不入库)');
  process.exit(2);
}

// T1:发现/QUIC 端口跟随 LOCALTRANS_TEST_PORT_BASE(与 core ports 模块同语义;
// 未设则默认 47600/47601)。用于预检端口占用。
const PORT_BASE = Number.parseInt(process.env.LOCALTRANS_TEST_PORT_BASE ?? '', 10);
const DISCOVERY_PORT = Number.isInteger(PORT_BASE) && PORT_BASE > 0 && PORT_BASE < 65535 ? PORT_BASE : 47600;
const QUIC_PORT = DISCOVERY_PORT + 1;

const conn = new Client();
const results = [];
const run = (cmd) => new Promise((res) => {
  conn.exec(cmd, { pty: false }, (err, stream) => {
    if (err) return res({ cmd, err: String(err) });
    let out = '', eout = '';
    stream.on('data', d => out += d).stderr.on('data', d => eout += d)
      .on('close', () => res({ cmd, out: out.trim(), eout: eout.trim() }));
  });
});

conn.on('ready', async () => {
  console.log(`[sshSetup] 已连接 ${USER}@${HOST}`);

  // —— 环境体检 ——
  const checks = [
    ['OS 版本', 'powershell -NoProfile -Command "(Get-CimInstance Win32_OperatingSystem).Caption + \' \' + (Get-CimInstance Win32_OperatingSystem).Version"'],
    ['空闲磁盘(C:)', 'powershell -NoProfile -Command "[math]::Round((Get-PSDrive C).Free/1GB)"'],
    ['WebView2', 'powershell -NoProfile -Command "if (Test-Path \'HKLM:\\\\SOFTWARE\\\\WOW6432Node\\\\Microsoft\\\\EdgeUpdate\\\\Clients\\\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}\') { \'installed\' } else { \'missing\' }"'],
    ['网卡列表', 'powershell -NoProfile -Command "(Get-NetAdapter | Where-Object Status -eq \'Up\').Name -join \', \' "'],
    ['本机 IP', 'powershell -NoProfile -Command "(Get-NetIPAddress -AddressFamily IPv4 | Where-Object {$_.IPAddress -like \'192.168.*\'}).IPAddress -join \',\'"'],
    ['是否管理员组', `net localgroup administrators | findstr /i ${USER}`],
    ['防火墙profile', 'netsh advfirewall show allprofiles state | findstr /i "启用 Enable"'],
    ['休眠状态', 'powercfg /a | findstr /i "休眠 Hibernate 待机 Standby"'],
    [`${DISCOVERY_PORT}/${QUIC_PORT}占用`, `netstat -ano | findstr /C:":${DISCOVERY_PORT}" /C:":${QUIC_PORT}"`],
    ['localtrans残留', 'tasklist | findstr /i localtrans'],
    // 回 ping 开发机(目标由 LT_SSH_PING_TARGET 传入;未设则跳过本项)
    ...(process.env.LT_SSH_PING_TARGET
      ? [['回 ping 开发机', `ping -n 2 -w 2000 ${process.env.LT_SSH_PING_TARGET} | findstr /i "TTL"`]]
      : []),
  ];
  for (const [name, cmd] of checks) {
    const r = await run(cmd);
    const val = (r.out || r.eout || '(空)').split('\n')[0];
    results.push([name, val]);
    console.log(`  ${name}: ${val}`);
  }

  // —— 安装公钥（普通用户路径 + 管理员路径都处理）——
  const setupCmd = [
    'if not exist %USERPROFILE%\\.ssh mkdir %USERPROFILE%\\.ssh',
    `powershell -NoProfile -Command "Add-Content -Path $env:USERPROFILE\\.ssh\\authorized_keys -Value '${PUBKEY.split(' ').slice(0,2).join(' ')} ${PUBKEY.split(' ')[2]}'"`,
    // 管理员用户时 sshd 默认只认 ProgramData 下文件；无权限则忽略报错
    `powershell -NoProfile -Command "try { Add-Content -Path C:\\ProgramData\\ssh\\administrators_authorized_keys -Value '${PUBKEY.split(' ').slice(0,2).join(' ')} ${PUBKEY.split(' ')[2]}' -ErrorAction Stop; icacls C:\\ProgramData\\ssh\\administrators_authorized_keys /inheritance:r /grant SYSTEM:F /grant BUILTIN\\Administrators:F } catch { 'no-admin-path' }"`,
  ].join(' && ');
  const r = await run(setupCmd);
  console.log(`[sshSetup] 公钥安装: ${(r.out || r.eout || '').split('\n').slice(-3).join(' | ')}`);

  conn.end();
  const fail = [];
  console.log('[sshSetup] 体检完成');
  if (fail.length) process.exit(1);
}).on('error', (e) => {
  console.error('[sshSetup] 连接失败:', e.message);
  process.exit(1);
}).connect({
  host: HOST, port: 22, username: USER, password: PW,
  readyTimeout: 15000,
});
