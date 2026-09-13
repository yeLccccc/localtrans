// Markdown 报告（spec §7.4.6）：场景×步骤×断言矩阵、失败详情、工件索引、环境信息。
// 发版回归 = 对报告做决策；报告落 reports/<runId>/report.md（gitignored）。
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';

/**
 * @param {object} r
 * @param {string} r.runId 编排器运行 id（目录名）
 * @param {string} r.scenario 场景名
 * @param {Array<{name:string,status:'PASS'|'FAIL'|'SKIP',detail?:string,durationMs?:number,artifacts?:string[]}>} r.steps
 * @param {object} r.env {targets, versions, startedAt, finishedAt, durationMs}
 * @param {string} r.outcome 'pass'|'fail'
 * @param {string} [r.failure] 失败摘要（顶层异常）
 * @returns {string} report.md 路径
 */
export function writeReport({ runId, scenario, steps, env, outcome, failure }) {
  const pass = steps.filter((s) => s.status === 'PASS').length;
  const fail = steps.filter((s) => s.status === 'FAIL').length;
  const lines = [];
  lines.push(`# ${scenario} · ${outcome === 'pass' ? '✅ PASS' : '❌ FAIL'}`);
  lines.push('');
  lines.push(`- 运行 ID：\`${runId}\``);
  lines.push(`- 结果：**${pass}/${steps.length} 步骤通过**（FAIL ${fail}）`);
  lines.push(`- 开始：${env.startedAt}　结束：${env.finishedAt}　耗时 ${((env.durationMs || 0) / 1000).toFixed(1)}s`);
  for (const [n, v] of Object.entries(env.versions || {})) {
    lines.push(`- ${n}：v${v.appVersion}（${v.buildProfile}，bridge ${v.bridgeReady ? 'ready' : 'NOT READY'}）`);
  }
  if (failure) {
    lines.push('');
    lines.push('> **失败**：' + failure);
  }
  lines.push('');
  lines.push('## 步骤矩阵');
  lines.push('');
  lines.push('| # | 步骤 | 状态 | 耗时 | 说明 | 工件 |');
  lines.push('|---|---|---|---|---|---|');
  steps.forEach((s, i) => {
    const mark = s.status === 'PASS' ? 'PASS' : s.status === 'FAIL' ? '**FAIL**' : 'SKIP';
    const dur = s.durationMs !== undefined ? `${(s.durationMs / 1000).toFixed(1)}s` : '';
    const detail = (s.detail || '').replace(/\|/g, '\\|').replace(/\r?\n/g, ' ');
    const arts = (s.artifacts || []).map((a) => `[${a.split('/').pop()}](${a})`).join(' ');
    lines.push(`| ${i + 1} | ${s.name} | ${mark} | ${dur} | ${detail} | ${arts} |`);
  });
  lines.push('');
  lines.push('## 工件索引');
  lines.push('');
  lines.push('- 操作流水：[journal.jsonl](journal.jsonl)');
  for (const s of steps) {
    for (const a of s.artifacts || []) {
      lines.push(`- [${s.name}] ${a}`);
    }
  }
  if (env.notes?.length) {
    lines.push('');
    lines.push('## 备注');
    lines.push('');
    for (const n of env.notes) lines.push(`- ${n}`);
  }
  const file = join(env.runDir, 'report.md');
  writeFileSync(file, lines.join('\n') + '\n', 'utf8');
  return file;
}
