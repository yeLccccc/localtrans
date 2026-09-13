// Journal（spec §7.4.3）：编排器全量操作流水，追加写 reports/<runId>/journal.jsonl。
// 作用：失败复盘精确回放操作序列；"人或 AI 重跑同样操作"的执行依据。
// 所有 Target.api 调用自动入账（时间戳/目标/方法路径/请求摘要/响应状态与摘要）。
import { mkdirSync, appendFileSync } from 'node:fs';
import { join } from 'node:path';

const MAX_STR = 240; // 摘要截断（journal 是流水不是数据面，防巨对象刷屏）

function brief(v) {
  if (v === undefined) return undefined;
  if (typeof v === 'string') return v.length > MAX_STR ? `${v.slice(0, MAX_STR)}…(${v.length})` : v;
  if (v === null || typeof v !== 'object') return v;
  const s = JSON.stringify(v);
  return s.length > MAX_STR ? `${s.slice(0, MAX_STR)}…(${s.length})` : JSON.parse(s);
}

export class Journal {
  /** @param runDir reports/<runId> 目录（已建或可建） */
  constructor(runDir) {
    this.runDir = runDir;
    this.file = join(runDir, 'journal.jsonl');
    this.seq = 0;
    mkdirSync(runDir, { recursive: true });
  }

  /**
   * 追加一条流水。entry 自由形态，常用字段：
   * { kind: 'api'|'event'|'note', target, method, path, status,
   *   req: 摘要, res: 摘要, error, elapsedMs }
   */
  append(entry) {
    this.seq += 1;
    const line = JSON.stringify({
      ts: new Date().toISOString(),
      seq: this.seq,
      ...entry,
      req: brief(entry.req),
      res: brief(entry.res),
      error: brief(entry.error),
    });
    appendFileSync(this.file, line + '\n');
    return this.seq;
  }

  /** api 调用入账（Target.api 自动调用） */
  api({ target, method, path, status, req, res, error, elapsedMs }) {
    return this.append({ kind: 'api', target, method, path, status, req, res, error, elapsedMs });
  }

  get path() {
    return this.file;
  }
}
