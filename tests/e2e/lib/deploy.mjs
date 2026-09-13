// 一键部署（M5 实证链路的脚本化，spec §7.4 部署节 + M5 执行记录关键发现）：
//   1. 停 huss_pc 本地实例（exe 被 Windows 锁映像，构建前必须杀）
//   2. npm --prefix ui run build:test（vite --mode test-api）
//   3. cargo build --release -p localtrans --features "test-api tauri/custom-protocol"
//      （必须带 tauri/custom-protocol：裸 build 产物仍指向 devUrl，测试机 webview 白屏）
//   4. huss_laptop：ssh 杀远端进程 → sftp 推 exe → schtasks /run /tn LT-Test
//      （ssh 会话属 session-0，GUI 须经交互式任务启动——任务已存在，勿重复 create）
//   5. 双端轮询 health bridgeReady=true
// huss_pc 本地启停：PowerShell Start-Process 脱离编排会话（bash 后台子进程随会话回收）。
//
// CLI：node lib/deploy.mjs [--skip-build] [--skip-huss_pc] [--skip-huss_laptop]
// 编程接口：import { deployAll, startPcA, stopPcA, restartPcB, ... }
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { Client } from 'ssh2';
import { loadTargets, repoRoot, e2eRoot } from './config.mjs';

const ROOT = repoRoot();
const EXE = join(ROOT, 'target', 'release', 'localtrans.exe');
const PCB_DIR = 'C:/Users/huss_laptop/localtrans-test';
const PCB_EXE = `${PCB_DIR}/localtrans.exe`;

const log = (m) => console.log(`[deploy] ${m}`);
const run = (cmd, args, opts = {}) => {
  log(`$ ${cmd} ${args.join(' ')}`);
  const r = spawnSync(cmd, args, {
    shell: true, stdio: 'inherit', cwd: opts.cwd || ROOT,
    timeout: opts.timeoutMs || 20 * 60_000, env: process.env,
  });
  if (r.status !== 0) throw new Error(`${cmd} 退出码 ${r.status}`);
  return r;
};

// ---------------------------------------------------------------------------
// SSH（ssh2，密钥认证；远端 Windows cmd）
// ---------------------------------------------------------------------------
function sshConnect(cfg) {
  const keyPath = resolve(e2eRoot(), cfg.ssh.key);
  if (!existsSync(keyPath)) throw new Error(`ssh 私钥不存在: ${keyPath}`);
  return new Promise((res, rej) => {
    const conn = new Client();
    conn.on('ready', () => res(conn))
      .on('error', (e) => rej(new Error(`ssh ${cfg.ssh.user}@${cfg.ssh.host}: ${e.message}`)))
      .connect({
        host: cfg.ssh.host, port: 22, username: cfg.ssh.user,
        privateKey: readFileSync(keyPath), readyTimeout: 15_000,
      });
  });
}

/** 远端执行（cmd.exe），返回 {code, out}；out 尽力 utf8 解码 */
export async function sshExec(cfg, cmd, { ignoreCode = false } = {}) {
  const conn = await sshConnect(cfg);
  try {
    return await new Promise((res, rej) => {
      conn.exec(cmd, { pty: false }, (err, stream) => {
        if (err) return rej(err);
        let out = '';
        stream.on('data', (d) => { out += d; })
          .stderr.on('data', (d) => { out += d; })
          .on('close', (code) => {
            const text = out.replace(/\r\n/g, '\n').trim();
            // Windows sshd 的通道常无 exit-status（code undefined）——有输出视为成功；
            // 仅显式非 0 退出码才判失败（schtasks/taskkill 成功输出是 GBK，不解析）
            if (typeof code === 'number' && code !== 0 && !ignoreCode) {
              rej(new Error(`远端命令失败(${code}): ${cmd}\n${text.slice(-500)}`));
            } else {
              res({ code: code ?? 0, out: text });
            }
          });
      });
    });
  } finally {
    conn.end();
  }
}

/** sftp 上传（fastPut，覆盖写） */
export async function sftpUpload(cfg, local, remote) {
  const conn = await sshConnect(cfg);
  try {
    return await new Promise((res, rej) => {
      conn.sftp((err, sftp) => {
        if (err) return rej(err);
        sftp.fastPut(local, remote, (e) => {
          sftp.end();
          if (e) rej(new Error(`sftp 上传失败 ${local} → ${remote}: ${e.message}`));
          else res({ local, remote });
        });
      });
    });
  } finally {
    conn.end();
  }
}

/** sftp 下载（fastGet，覆盖写）——L3 场景备份 huss_laptop data/ 文件用 */
export async function sftpDownload(cfg, remote, local) {
  const conn = await sshConnect(cfg);
  try {
    return await new Promise((res, rej) => {
      conn.sftp((err, sftp) => {
        if (err) return rej(err);
        sftp.fastGet(remote, local, (e) => {
          sftp.end();
          if (e) rej(new Error(`sftp 下载失败 ${remote} → ${local}: ${e.message}`));
          else res({ remote, local });
        });
      });
    });
  } finally {
    conn.end();
  }
}

// ---------------------------------------------------------------------------
// 构建与启停
// ---------------------------------------------------------------------------
export function buildUi() {
  log('构建前端测试包 (vite --mode test-api)…');
  run('npm', ['--prefix', 'ui', 'run', 'build:test']);
}

export function buildRust() {
  log('构建 Rust 测试产物 (test-api + custom-protocol)…');
  run('cargo', ['build', '--release', '-p', 'localtrans', '--features', '"test-api tauri/custom-protocol"']);
}

/** 杀本地 localtrans.exe（不存在时 taskkill 非 0，忽略） */
export function stopPcA() {
  log('停止 huss_pc 本地实例…');
  spawnSync('taskkill', ['/F', '/IM', 'localtrans.exe'], { shell: true });
  // 确认 pid 已死再返回（规避 single-instance 转发旧窗口）
  for (let i = 0; i < 10; i++) {
    const r = spawnSync('tasklist', ['/FI', 'IMAGENAME eq localtrans.exe'], { shell: true, encoding: 'utf8' });
    if (!(r.stdout || '').toLowerCase().includes('localtrans.exe')) return;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 500);
  }
  throw new Error('huss_pc 进程未在 5s 内退出');
}

/** Start-Process 脱离编排会话启动 huss_pc（env 经父进程继承传入） */
export function startPcA(targets = loadTargets()) {
  if (!existsSync(EXE)) throw new Error(`产物不存在: ${EXE}（先跑构建）`);
  log(`启动 huss_pc 本地实例 (${EXE}, 端口 ${targets.huss_pc.port})…`);
  const ps = [
    `$env:LOCALTRANS_TEST_API='1'`,
    `$env:LOCALTRANS_TEST_API_KEY='${targets.huss_pc.token}'`,
    `$env:LOCALTRANS_TEST_API_PORT='${targets.huss_pc.port}'`,
    `Start-Process -FilePath '${EXE}'`,
  ].join('; ');
  const r = spawnSync('powershell', ['-NoProfile', '-Command', ps], { shell: true, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`Start-Process 失败: ${r.stderr || r.stdout}`);
}

export async function stopPcB(targets = loadTargets()) {
  log('停止 huss_laptop 远端实例…');
  const { out } = await sshExec(targets.huss_laptop, 'taskkill /F /IM localtrans.exe', { ignoreCode: true });
  log(`  taskkill: ${out.split('\n').pop() || '(无输出)'}`);
}

/** 部署 huss_laptop：杀进程 → 推 exe → schtasks 交互式启动（任务 LT-Test 已存在，勿 create） */
export async function deployPcB(targets = loadTargets()) {
  if (!existsSync(EXE)) throw new Error(`产物不存在: ${EXE}（先跑构建）`);
  await stopPcB(targets);
  log(`上传 exe → ${PCB_EXE} …`);
  await sftpUpload(targets.huss_laptop, EXE, PCB_EXE);
  log('schtasks /run /tn LT-Test …');
  await sshExec(targets.huss_laptop, 'schtasks /run /tn LT-Test');
}

/** 轮询 health(免认证) 直到 200 且 version(带 Token).bridgeReady=true */
export async function waitReady({ host, port, token }, { timeoutMs = 120_000, label = 'target' } = {}) {
  const deadline = Date.now() + timeoutMs;
  const base = `http://${host}:${port}`;
  const auth = token ? { Authorization: `Bearer ${token}` } : {};
  let lastErr = '';
  while (Date.now() < deadline) {
    try {
      const h = await fetch(`${base}/api/health`, { signal: AbortSignal.timeout(2000) });
      if (h.ok) {
        const r = await fetch(`${base}/api/version`, { headers: auth, signal: AbortSignal.timeout(2000) });
        const v = await r.json();
        if (v?.data?.bridgeReady === true) {
          log(`${label} 就绪 (v${v.data.appVersion}, ${v.data.buildProfile})`);
          return v.data;
        }
        lastErr = r.status === 401 ? 'version 401（token 不匹配）' : `bridgeReady=${v?.data?.bridgeReady}`;
      } else {
        lastErr = `health ${h.status}`;
      }
    } catch (e) {
      lastErr = String(e.message || e);
    }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 1000);
  }
  throw new Error(`${label} ${timeoutMs}ms 内未就绪（最后状态: ${lastErr}）`);
}

/** 全流程部署。opts: {skipBuild, skipPcA, skipPcB} */
export async function deployAll(opts = {}) {
  const targets = loadTargets();
  const t0 = Date.now();
  if (!opts.skipPcA) stopPcA(); // 先杀本地实例再构建（exe 文件锁）
  if (!opts.skipBuild) {
    buildUi();
    buildRust();
  }
  if (!opts.skipPcB) await deployPcB(targets);
  if (!opts.skipPcA) startPcA(targets);
  await waitReady(targets.huss_laptop, { label: 'huss_laptop' });
  await waitReady(targets.huss_pc, { label: 'huss_pc' });
  log(`完成，耗时 ${((Date.now() - t0) / 1000).toFixed(1)}s`);
  return targets;
}

// CLI 直跑
if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const args = process.argv.slice(2);
  deployAll({
    skipBuild: args.includes('--skip-build'),
    skipPcA: args.includes('--skip-huss_pc'),
    skipPcB: args.includes('--skip-huss_laptop'),
  }).catch((e) => { console.error(`[deploy] 失败: ${e.message}`); process.exit(1); });
}
