// perfreport.mjs — P4 性能基线报告再生（docs/audit/perf-baseline.md）。
// 三个 perf 场景（perf-baseline / perf-render / perf-idle）结束时各自调用
// regeneratePerfReport(me，meResult)：扫 reports/ 下三个场景各自的最新
// result.json，全量重写基线报告——幂等，后跑的场景刷新自己的段落，
// 同时保留其它场景最近一次绿跑的数据（标记其日期，过期可辨）。
import { existsSync, readdirSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { e2eRoot, repoRoot } from './config.mjs';

const MB = 1048576; // 报告口径：1 MB = 1 MiB = 1048576 字节

/** reports/ 下某场景前缀的最新 result.json；无 → null */
export function latestResult(scenarioPrefix) {
  const reports = join(e2eRoot(), 'reports');
  if (!existsSync(reports)) return null;
  const dirs = readdirSync(reports).filter((d) => d.startsWith(scenarioPrefix)).sort();
  for (let i = dirs.length - 1; i >= 0; i--) {
    const f = join(reports, dirs[i], 'result.json');
    if (existsSync(f)) {
      try { return { evidenceDir: `reports/${dirs[i]}`, ...JSON.parse(readFileSync(f, 'utf8')) }; }
      catch { /* 损坏文件当不存在 */ }
    }
  }
  return null;
}

const fmtMB = (b) => (b === null || b === undefined) ? '—' : (b / MB).toFixed(1);
const fmtDate = (iso) => (iso || '').replace('T', ' ').slice(0, 16);

/** T1/T2 段（perf-baseline result） */
function sectionBaseline(r) {
  if (!r) return '> 未测（perf-baseline 尚无绿跑工件）\n';
  const med = r.median || {};
  const rows = (r.runs || []).map((x) =>
    `| ${x.run} | ${x.name} | ${x.sizeBytes} | ${(x.durationMs / 1000).toFixed(1)}s | ${x.mbps.toFixed(1)} | ` +
    `${x.cpuAvgPercent === null ? '—' : x.cpuAvgPercent.toFixed(1)} | ${fmtMB(x.memWorkingSetBytes)} |`).join('\n');
  const batch = r.batch;
  return `
| 轮次 | 文件 | 字节 | 耗时 | 全程 MB/s | 发送端 CPU 均值（单核%） | 结束内存 MB |
|---|---|---|---|---|---|---|
${rows}
| **中位** | — | ${med.sizeBytes ?? '—'} | ${med.durationMs ? (med.durationMs / 1000).toFixed(1) + 's' : '—'} | **${med.mbps?.toFixed(1) ?? '—'}** | ${med.cpuAvgPercent === null || med.cpuAvgPercent === undefined ? '—' : med.cpuAvgPercent.toFixed(1)} | ${fmtMB(med.memWorkingSetBytes)} |

**T2 小文件批量（200×10KB 单 offer）**：${batch ? `耗时 ${(batch.totalMs / 1000).toFixed(2)}s，吞吐 ${batch.mbps.toFixed(1)} MB/s（${batch.bytesTotal} 字节，父卡 ${batch.state}）` : '未记录'}

对照与观测：
- huss_laptop 环回对照：${r.laptopLoopback ? `1×500MB ${r.laptopLoopback.mbps.toFixed(1)} MB/s（PC-PC LAN，非主口径）` : '条件跳过（设备不可达/未配对——中继与双机口径留遗留）'}
- 中继口径：${r.relayNote || '未观测'}
${(r.observations || []).map((o) => `- ${o}`).join('\n')}
`.trim();
}

/** T3 段（perf-render result） */
function sectionRender(r) {
  if (!r) return '> 未测（perf-render 尚无绿跑工件）\n';
  return `| 指标 | 值 | 断言 |
|---|---|---|
| 历史卡数 | ${r.historyCount} | ≥1000 |
| 1000 卡展开首屏（点击→末卡 uiWait 命中） | ${r.renderMs} ms | <2000 ms ✅ |
| 页面导航首屏（navigate→折叠条可见） | ${r.navMs} ms | 记录值 |
| 交互 DOM 节点数（uiTree 可见交互元素口径） | ${r.domInteractiveNodes} | ≥1000 |

滚动流畅度：人工目检项（webview 无程序化滚动注入面），证据截图 ${r.evidenceDir} 目检。`.trim();
}

/** T4 段（perf-idle result） */
function sectionIdle(r) {
  if (!r) return '> 未测（perf-idle 尚无绿跑工件）\n';
  const spikes = (r.samples || []).filter((s) => (s.cpuPercent ?? 0) > 5).length;
  const series = (r.samples || []).map((s, i) => `${i * 30}s:${s.cpuPercent === null ? '—' : s.cpuPercent.toFixed(1)}%`).join(' ');
  return `| 指标 | 值 | 断言 |
|---|---|---|
| CPU 均值（单核口径） | ${r.meanPercent.toFixed(2)}% | <2% ✅ |
| CPU 峰值 | ${r.maxPercent === null ? '—' : r.maxPercent.toFixed(1)}% | >5% 单点 ${spikes} 个（${spikes > 0 ? '发现周期扫描疑点，见备注' : '无异常'}） |
| 内存（起→末） | ${fmtMB(r.memStartBytes)} → ${fmtMB(r.memEndBytes)} MB | 记录值 |

采样序列（每 30s）：${series}
${r.note ? `\n备注：${r.note}` : ''}`.trim();
}

/**
 * 再生 docs/audit/perf-baseline.md。
 * @param {string} me 当前调用方场景前缀（其段落用传入的新鲜结果）
 * @param {object} meResult 当前跑的 result 本体（不含 evidenceDir 也可）
 */
export function regeneratePerfReport(me, meResult = null) {
  const fresh = meResult ? { evidenceDir: '', ...meResult } : null;
  const baseline = me === 'perf-baseline' ? fresh : latestResult('perf-baseline');
  const render = me === 'perf-render' ? fresh : latestResult('perf-render');
  const idle = me === 'perf-idle' ? fresh : latestResult('perf-idle');

  const dates = [baseline?.finishedAt, render?.finishedAt, idle?.finishedAt].filter(Boolean).sort();
  const date = dates.length ? fmtDate(dates[dates.length - 1]) : '（无数据）';

  const md = `# 性能基线报告（P4）

> 由 e2e perf 场景自动再生（tests/e2e/lib/perfreport.mjs）——手工勿改，复跑即刷新。
> 三个段落各自标注最近一次绿跑日期；场景失败不覆盖旧数据。

- **口径**：PC→手机 **WiFi 实测**（真实用户主路径；手机 USB 仅作 adb 控制面，传输走 QUIC/WiFi）。
  huss_laptop PC-PC LAN 为可选对照段；中继口径留双机遗留（条件跳过，不部署中继）。
- **日期**：${date}
- **环境**：huss_pc（release 测试构建，test-api）+ huss_phone（M2002J9E，MIUI 12）。
- **单位**：1 MB = 1 MiB = ${MB} 字节；CPU% 为单核口径（100% = 跑满 1 核，Win32 PerfProc PercentProcessorTime）。
- **证据说明**：测试时段开发机屏幕锁定时，窗口截图（GDI 桌面裁剪）拍到的是锁屏壁纸——
  渲染正确性以 DOM 断言（uiTree/uiWait）为准，截图仅作过程证据（shot.rs 设计原则）。

## T1 大文件吞吐 + T2 小文件批量（perf-baseline，${fmtDate(baseline?.finishedAt) || '未测'}）

${sectionBaseline(baseline)}

## T3 前端渲染 1000 卡（perf-render，${fmtDate(render?.finishedAt) || '未测'}）

${sectionRender(render)}

## T4 空闲足迹 5 分钟（perf-idle，${fmtDate(idle?.finishedAt) || '未测'}）

${sectionIdle(idle)}

## 复跑方式

\`\`\`bash
cd tests/e2e && node lib/deploy.mjs --skip-huss_laptop   # PC 测试构建
node scenarios/perf-baseline.mjs   # T1+T2（需手机在线；不可达自动条件跳过）
node scenarios/perf-render.mjs     # T3（PC 单机）
node scenarios/perf-idle.mjs       # T4（PC 单机，约 6 分钟）
\`\`\`

回归判定：与本基线比，吞吐 ±20% 标红（人工/评审口径；场景断言只锁完成性与上限，
不锁具体速率，避免 WiFi 波动假失败）。
`;
  const out = join(repoRoot(), 'docs', 'audit', 'perf-baseline.md');
  mkdirSync(join(repoRoot(), 'docs', 'audit'), { recursive: true });
  writeFileSync(out, md, 'utf8');
  return out;
}
