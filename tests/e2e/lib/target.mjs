// Target 统一抽象（spec §7.4.2）：一台被测设备的 test-api 客户端。
// huss_pc/huss_laptop 同构（HTTP over LAN），Android 后续在此接口上扩展（M7/M8）。
// 所有 api() 调用自动入 journal；ui/state/invoke/screenshot 便捷方法
// 基于 api() 组合，错误统一抛 ApiError（带 status/code/detail 便于报告）。
import { writeFileSync, mkdirSync } from 'node:fs';
import { dirname } from 'node:path';

export class ApiError extends Error {
  constructor(msg, { status, code, detail, target, path }) {
    super(msg);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.detail = detail;
    this.target = target;
    this.path = path;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export class Target {
  /**
   * @param {string} name 目标名（huss_pc/huss_laptop，journal/证据文件命名用）
   * @param {{host:string, port:string|number, token:string}} cfg targets.local.yaml 块
   * @param {import('./journal.mjs').Journal} journal 入账（可空）
   */
  constructor(name, cfg, journal = null) {
    this.name = name;
    this.cfg = cfg;
    this.base = `http://${cfg.host}:${cfg.port}`;
    this.journal = journal;
    this.runId = null; // test/begin 后记录
  }

  /** 裸 api 调用：入 journal，返回 {status, json, bin, headers}；网络错误也入账后上抛 */
  async api(path, opts = {}) {
    const method = opts.method || (opts.body ? 'POST' : 'GET');
    const started = Date.now();
    let out;
    try {
      const r = await fetch(`${this.base}${path}`, {
        ...opts,
        headers: {
          Authorization: `Bearer ${this.cfg.token}`,
          ...(opts.body ? { 'content-type': 'application/json' } : {}),
          ...(opts.headers || {}),
        },
        signal: AbortSignal.timeout(opts.timeoutMs || 180_000),
      });
      const ct = r.headers.get('content-type') || '';
      out = {
        status: r.status,
        json: ct.includes('json') ? await r.json() : null,
        bin: ct.includes('png') ? Buffer.from(await r.arrayBuffer()) : null,
        headers: r.headers,
      };
    } catch (e) {
      this.journal?.api({
        target: this.name, method, path, status: 0, req: bodyBrief(opts.body),
        error: String(e), elapsedMs: Date.now() - started,
      });
      throw new ApiError(`${this.name} ${path} 网络错误: ${e}`, { status: 0, target: this.name, path });
    }
    this.journal?.api({
      target: this.name, method, path, status: out.status, req: bodyBrief(opts.body),
      res: out.json ? (out.json.ok ? out.json.data : out.json.error) : `<png ${out.bin?.length}B>`,
      elapsedMs: Date.now() - started,
    });
    return out;
  }

  /** 断言 200 + ok 包络，返回 data；否则抛 ApiError（带契约错误码） */
  async ok(path, opts = {}) {
    const r = await this.api(path, opts);
    if (r.status !== 200 || !r.json?.ok) {
      const err = r.json?.error || {};
      throw new ApiError(
        `${this.name} ${path} → ${r.status} ${err.code || ''} ${err.message || ''}`.trim(),
        { status: r.status, code: err.code, detail: err.detail, target: this.name, path },
      );
    }
    return r.json.data;
  }

  post(path, body) {
    return this.ok(path, { method: 'POST', body: JSON.stringify(body || {}) });
  }

  // ---- 版本/健康 ----
  health() { return this.ok('/api/health'); }
  async version() { return this.ok('/api/version'); }

  // ---- test/run 关联 ----
  async beginTest(scenario) {
    const d = await this.post('/api/test/begin', { scenario });
    this.runId = d.runId;
    return d.runId;
  }
  step(name) { return this.post('/api/test/step', { name }); }
  endTest(outcome) { return this.post('/api/test/end', { runId: this.runId, outcome }); }

  // ---- 状态断言 ----
  state() { return this.ok('/api/state/transfers'); }
  stateApp() { return this.ok('/api/state/app'); }
  /**
   * state/wait：等待条件满足。超时（408）抛 ApiError（detail.lastValue 带末次观测）。
   * @returns {Promise<{matched:true, observed, polls, elapsedMs}>}
   */
  async waitState({ source = 'transfers', path, op, value, timeoutMs = 10_000, intervalMs = 500 }) {
    const body = { source, path, op, timeoutMs, intervalMs };
    if (value !== undefined) body.value = value;
    return this.post('/api/state/wait', body);
  }

  // ---- UI 桥 ----
  uiTree() { return this.ok('/api/ui/tree'); }
  uiNavigate(path) { return this.post('/api/ui/navigate', { path }); }
  uiClick(selector) { return this.post('/api/ui/click', { selector }); }
  uiToggle(selector) { return this.post('/api/ui/toggle', { selector }); }
  uiInput(selector, value, opts = {}) {
    const body = { selector, value };
    if (opts.clear !== undefined) body.clear = opts.clear;
    if (opts.events) body.events = opts.events;
    return this.post('/api/ui/input', body);
  }
  async uiText(selector) {
    return this.ok(`/api/ui/text${selector ? `?selector=${encodeURIComponent(selector)}` : ''}`);
  }
  uiWait(selector, timeoutMs = 10_000) {
    return this.post('/api/ui/wait', { selector, timeoutMs });
  }
  uiWaitText(text, timeoutMs = 10_000) {
    return this.post('/api/ui/wait', { text, timeoutMs });
  }

  // ---- invoke 白名单 ----
  invoke(cmd, args = {}) { return this.post('/api/invoke', { cmd, args }); }

  // ---- 日志/截图 ----
  logs({ runId, afterSeq, level, target } = {}) {
    const q = new URLSearchParams();
    if (runId) q.set('runId', runId);
    if (afterSeq !== undefined) q.set('afterSeq', String(afterSeq));
    if (level) q.set('level', level);
    if (target) q.set('target', target);
    const qs = q.toString();
    return this.ok(`/api/logs/tail${qs ? `?${qs}` : ''}`);
  }

  /** 截图并存盘；返回 {file, width, height, scale, bytes} */
  async screenshot(file, { restore = true, soft = false } = {}) {
    const r = await this.api(`/api/screenshot${restore ? '' : '?restore=false'}`).catch((e) => e);
    if (r.status !== 200 || !r.bin) {
      // soft 模式:截图是证据不是断言(shot.rs 设计原则)——显示器离位等
      // 环境问题(如 schtasks 会话枚举不到显示器)不致死场景,记警告继续
      const msg = `${this.name} 截图失败: ${r.status ?? '网络错误'}`;
      if (soft) { console.warn(`[warn] ${msg}(soft)`); return { file, skipped: true }; }
      throw new ApiError(msg, { status: r.status, target: this.name, path: '/api/screenshot' });
    }
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, r.bin);
    return {
      file,
      width: Number(r.headers.get('x-width') || 0),
      height: Number(r.headers.get('x-height') || 0),
      scale: r.headers.get('x-scale'),
      bytes: r.bin.length,
    };
  }

  /** screenshot 的软别名(证据用,环境性失败降级为警告) */
  screenshotSoft(file, opts = {}) { return this.screenshot(file, { ...opts, soft: true }); }

  /** 关掉断点恢复提示弹窗(若在)——L2 重启后 interrupted 卡会触发,
   * 挡住 offer-modal 等后续 UI。幂等:无弹窗时静默。 */
  async dismissResumePrompt() {
    try {
      await this.uiWait('[testid=resume-ignore-btn]', 2500);
      await this.uiClick('[testid=resume-ignore-btn]');
      await new Promise((r) => setTimeout(r, 500));
      return true;
    } catch { return false; }
  }

  /** 轮询直至 fetch 满足 pred（快照级断言，waitState 表达不了的"存在性"用） */
  async pollUntil(pred, { timeoutMs = 20_000, intervalMs = 500, what = '条件' } = {}) {
    const deadline = Date.now() + timeoutMs;
    let last;
    for (;;) {
      last = await this.state();
      const hit = pred(last);
      if (hit) return { snapshot: last, value: hit };
      if (Date.now() >= deadline) {
        throw new ApiError(`${this.name} 轮询超时（${timeoutMs}ms）: ${what}`, {
          status: 0, target: this.name, path: '<pollUntil>', detail: { last },
        });
      }
      await sleep(intervalMs);
    }
  }
}

function bodyBrief(b) {
  if (!b) return undefined;
  try { return JSON.parse(b); } catch { return String(b); }
}
