// 证据收割（spec §7.4.4）：任一步失败自动打包——双端 runId 范围全量日志 +
// 双端截图 + 双端状态快照 → reports/<runId>/evidence/<step>/。
// 每个工件相对 runDir 落盘，报告与 journal 用相对链接引用。
import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

/**
 * 收割双端证据。
 * @param {import('./target.mjs').Target[]} targets
 * @param {string} runDir reports/<runId>
 * @param {string} stepLabel 步骤标签（目录名用，非法字符替换）
 * @returns {Promise<Array<{target, files: string[]}>>} 相对 runDir 的工件清单
 */
export async function collectEvidence(targets, runDir, stepLabel) {
  const safe = stepLabel.replace(/[^\w.-]+/g, '_').slice(0, 60);
  const dir = join(runDir, 'evidence', safe);
  mkdirSync(dir, { recursive: true });
  const manifest = [];
  for (const t of targets) {
    const files = [];
    // 单目标收割失败不阻断其余目标（证据采集本身要皮实）
    try {
      const shot = await t.screenshotSoft(join(dir, `${t.name}.png`));
      files.push(`evidence/${safe}/${t.name}.png`);
      void shot;
    } catch (e) { console.warn(`[evidence] ${t.name} 截图失败: ${e.message}`); }
    try {
      const st = await t.state();
      writeFileSync(join(dir, `${t.name}-state.json`), JSON.stringify(st, null, 2));
      files.push(`evidence/${safe}/${t.name}-state.json`);
    } catch (e) { console.warn(`[evidence] ${t.name} 状态快照失败: ${e.message}`); }
    try {
      const lg = await t.logs({ runId: t.runId || undefined });
      writeFileSync(
        join(dir, `${t.name}-logs.json`),
        JSON.stringify(lg, null, 2),
      );
      files.push(`evidence/${safe}/${t.name}-logs.json`);
    } catch (e) { console.warn(`[evidence] ${t.name} 日志提取失败: ${e.message}`); }
    manifest.push({ target: t.name, files });
  }
  return manifest;
}
