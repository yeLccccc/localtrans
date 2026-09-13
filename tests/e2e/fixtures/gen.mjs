// fixtures 生成脚本（spec §7.4.1 目录契约）：确定性内容，可重复生成校验。
//   small-1kb.bin / medium-5mb.bin / chinese-中文名测试.txt / empty.txt
//   node fixtures/gen.mjs --large 追加 large-500mb.bin（默认不建，按需手动）
// *.bin 不入库（fixtures/.gitignore）；本脚本与 .txt fixtures 入库。
import { mkdirSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const DIR = dirname(fileURLToPath(import.meta.url));
mkdirSync(DIR, { recursive: true });

/** 确定性伪随机字节（xorshift32，种子即 size——同尺寸同内容，便于比对） */
function bytes(size, seed = 0x4c54) {
  const buf = Buffer.allocUnsafe(size);
  let x = seed ^ size;
  for (let i = 0; i < size; i++) {
    x ^= x << 13; x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5; x >>>= 0;
    buf[i] = x & 0xff;
  }
  return buf;
}

export function genAll({ large = false } = {}) {
  const made = [];
  const mk = (name, data) => {
    const p = join(DIR, name);
    writeFileSync(p, data);
    made.push({ name, size: data.length });
  };
  mk('small-1kb.bin', bytes(1024));
  mk('medium-5mb.bin', bytes(5 * 1024 * 1024));
  mk('empty.txt', Buffer.alloc(0));
  mk('chinese-中文名测试.txt', Buffer.from([
    '中文名传输测试文件（fixtures/gen.mjs 生成，内容固定）',
    '行2：ASCII mixed 中文 émoji 转义 \\ / : * ? " < > |',
    `行3：生成时间无关——内容确定性，禁手改。`,
  ].join('\n'), 'utf8'));
  if (large) mk('large-500mb.bin', bytes(500 * 1024 * 1024));
  return made;
}

/** fixture 绝对路径（pcA 推送用）；缺失即生成 */
export function ensureFixtures() {
  const need = ['small-1kb.bin', 'medium-5mb.bin', 'empty.txt', 'chinese-中文名测试.txt'];
  const missing = need.filter((n) => !existsSync(join(DIR, n)));
  if (missing.length) genAll();
  return need.map((n) => join(DIR, n));
}

/**
 * 运行期唯一变体（M6）：在 fixtures/runs/<runId>/ 生成 4 个文件——名字与
 * 基准 fixture 一致、内容尾部追加 runId 戳。原因：接收端按内容去重（秒传），
 * 重复推送同内容会走 instant 路径（不建卡/卡秒收敛），"4 文件真传输"断言
 * 不确定；变体保证每次运行都是全量真实传输（秒传/去重行为留给独立场景）。
 * 注意 empty 变体不再 0 字节（追加戳）——空文件传输已由基准 fixture 首推覆盖。
 */
export function genRunVariants(runId) {
  const runDir = join(DIR, 'runs', runId);
  mkdirSync(runDir, { recursive: true });
  const stamp = Buffer.from(`\n[run ${runId}]\n`, 'utf8');
  const out = [];
  const mk = (name, base) => {
    const p = join(runDir, name);
    const data = base === null ? stamp : Buffer.concat([readFileSync(join(DIR, base)), stamp]);
    writeFileSync(p, data);
    out.push(p);
  };
  mk('small-1kb.bin', 'small-1kb.bin');
  mk('medium-5mb.bin', 'medium-5mb.bin');
  mk('chinese-中文名测试.txt', 'chinese-中文名测试.txt');
  mk('empty.txt', null); // 0 字节基准 + 戳 = 仅戳内容
  return out;
}

// CLI 直跑
if (process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url)) {
  const made = genAll({ large: process.argv.includes('--large') });
  for (const m of made) console.log(`[fixtures] ${m.name}  ${m.size} B`);
}
