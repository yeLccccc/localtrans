// 编排器配置：targets.local.yaml 行级解析（M5 smoke-m5 内联版抽公共模块）。
// yaml 结构刻意保持两层（顶层设备键 + 一层属性 + ssh 二级块），不引 yaml 依赖。
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** 解析一段顶层块：返回该块下一层的 `键: 值` 平面对象（更深层级跳过）。 */
export function parseBlock(lines, block) {
  const start = lines.indexOf(block);
  if (start === -1) throw new Error(`targets 配置缺少 ${block} 块`);
  const out = {};
  for (const l of lines.slice(start + 1)) {
    if (/^\S/.test(l)) break; // 下一个顶层键，块结束
    const m = l.match(/^ {2}(\w+):\s*(.*)$/); // 仅本层键值（更深缩进跳过）
    if (m) out[m[1]] = m[2].trim();
  }
  return out;
}

/** 解析二级块（如 `huss_laptop:` 下的 `  ssh:`）：返回 {键: 值}，缺失返回 {}。 */
export function parseSubBlock(lines, block, sub) {
  const start = lines.indexOf(block);
  if (start === -1) return {};
  const head = `  ${sub}:`;
  const rel = lines.slice(start + 1).indexOf(head);
  if (rel === -1) return {};
  const out = {};
  for (const l of lines.slice(start + 1 + rel + 1)) {
    if (/^ {0,2}\S/.test(l)) break; // 回到一/顶层即子块结束
    const m = l.match(/^ {4}(\w+):\s*(.*)$/);
    if (m) out[m[1]] = m[2].trim();
  }
  return out;
}

/**
 * 读取 targets.local.yaml（gitignored）。返回：
 * { huss_pc: {name,host,port,token}, huss_laptop: {..., ssh:{host,user,key}}, huss_phone: {...}, raw }
 * 设备命名规范（用户定）：huss_pc=开发机(本机) / huss_laptop=测试机 / huss_phone=Android 真机。
 * 校验必备字段（host/port/token），缺失即抛错（fail fast）。
 */
export function loadTargets(file) {
  const path = file
    ? fileURLToPath(file instanceof URL ? file : new URL(`file://${resolveInput(file)}`))
    : fileURLToPath(new URL('../targets.local.yaml', import.meta.url));
  const raw = readFileSync(path, 'utf8');
  const lines = raw.split(/\r?\n/);
  const pick = (block) => {
    const o = parseBlock(lines, `${block}:`);
    for (const k of ['host', 'port', 'token']) {
      if (!o[k]) throw new Error(`targets.local.yaml 的 ${block} 块缺少 ${k}`);
    }
    return o;
  };
  const huss_pc = pick('huss_pc');
  const huss_laptop = pick('huss_laptop');
  const ssh = parseSubBlock(lines, 'huss_laptop:', 'ssh');
  huss_laptop.ssh = {
    host: ssh.host || huss_laptop.host,
    user: ssh.user || '',
    key: ssh.key || 'ssh/id_ed25519',
  };
  if (!huss_laptop.ssh.user) throw new Error('targets.local.yaml 的 huss_laptop.ssh 块缺少 user');
  const huss_phone = parseBlock(lines, 'huss_phone:');
  return { huss_pc, huss_laptop, huss_phone, raw };
}

function resolveInput(file) {
  // 相对路径按 tests/e2e 目录解析（编排器惯例 cwd）
  return file.match(/^[A-Za-z]:[\\/]/) ? file : new URL(`../${file}`, import.meta.url).pathname;
}

/** 仓库根（tests/e2e/../../）绝对 Windows 路径。 */
export function repoRoot() {
  return stripSep(fileURLToPath(new URL('../../..', import.meta.url)));
}

/** tests/e2e 目录绝对 Windows 路径（带尾分隔符，供拼路径）。 */
export function e2eRoot() {
  return stripSep(fileURLToPath(new URL('../', import.meta.url)));
}

function stripSep(p) {
  return p.replace(/[\\/]$/, '');
}
