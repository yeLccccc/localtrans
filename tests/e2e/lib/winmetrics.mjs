// winmetrics.mjs — Windows 进程指标采样（P4 性能基线卡 T1/T4 用）。
// 走 CIM（Get-CimInstance）而非 Get-Counter/typeperf：计数器路径在本地化
// Windows（\Process vs \进程）上会失效，CIM 类名与 locale 无关。
// 口径：PercentProcessorTime 是"单核口径"——100% = 跑满 1 个核心
//（多核进程可 >100%），报告与断言均按此口径注明。
// 所有函数失败返回 null（尽力采样，不致死场景）。
import { execFile } from 'node:child_process';

function ps(script, timeoutMs = 8000) {
  return new Promise((resolve) => {
    execFile('powershell', ['-NoProfile', '-NonInteractive', '-Command', script],
      { timeout: timeoutMs, windowsHide: true, encoding: 'utf8' },
      (err, stdout) => resolve(err ? null : String(stdout).trim()));
  });
}

/** 空输出（进程不存在）与解析失败都归一为 null——与真实的 0% 可区分 */
function numOrNull(out) {
  if (out === null || out === '') return null;
  const v = Number(out);
  return Number.isFinite(v) ? v : null;
}

/**
 * 进程 CPU%（单核口径，100%=1 核）。进程不存在/采样失败 → null。
 * @param {string} procName 不带扩展名的进程名（CIM PerfProc_Process 口径）
 */
export async function sampleProcessCpu(procName = 'localtrans') {
  return numOrNull(await ps(
    `Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "Name='${procName}'" | Select-Object -First 1 -ExpandProperty PercentProcessorTime`));
}

/**
 * 进程工作集字节数。进程不存在/采样失败 → null。
 * @param {string} procName 带扩展名的进程名（Win32_Process 口径）
 */
export async function processWorkingSetBytes(procName = 'localtrans.exe') {
  return numOrNull(await ps(
    `Get-CimInstance Win32_Process -Filter "Name='${procName}'" | Select-Object -First 1 -ExpandProperty WorkingSetSize`));
}
