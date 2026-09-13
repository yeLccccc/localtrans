#!/usr/bin/env node
/**
 * Task M4 冒烟：证据收尾闭环（screenshot + invoke 白名单 + test/begin-step-end）
 *（spec §7.1.3 / §7.1.7-3 / §7.1.8 / §7.1.9，plan Task M4）
 *
 * 这是体系第一条 dogfooding 回归：把 M0-M4 全部端点串成一次完整测试 run。
 * 流程（步骤级 PASS/FAIL，任一失败退出码 1）：
 *   1. 清理：杀遗留 localtrans.exe、释放 1420/39871 端口（可重复运行）
 *   2. 后台起 vite dev --mode test-api（ui 1420，前端 __testBridge）
 *   3. cargo build -p localtrans --features test-api（增量）
 *   4. LOCALTRANS_TEST_API=1 KEY=smoketest PORT=39871 启动 debug exe（窗口会弹出）
 *   5. health → version（appVersion/apiVersion/bridgeReady/buildProfile）
 *   6. test/begin {scenario} → runId（32 位 hex）
 *   7. test/step {name} → 步骤标记
 *   8. ui/navigate /settings → ui/wait 文本"共享区"（M2 验证过的设置页锚点）
 *   9. ui/click [testid=nav-transfers-link]（无害：仅路由跳转，M2 同款）
 *  10. state/transfers（schemaVersion=1）→ state/wait 永真条件（gte 0 首拍即中）
 *  11. screenshot → PNG 魔数字节 + X-Width/X-Height/X-Scale 头，
 *      存 tests/e2e/reports/m4-<时间戳>/shot-1.png（断言 >10KB）
 *  12. logs/tail?runId= → 按 run 过滤出 begin/step 标记（run/step 关联通了）
 *  13. invoke 白名单内 get_settings → 200；白名单外 remove_transfer →
 *      403 INVOKE_NOT_ALLOWED 且 detail.allowed 非空
 *  14. test/end {runId, outcome} → 统计条数；负路径：错 runId / 无活动 run → 400
 *  15. fallback：未知路径 → 404 NOT_FOUND 包络 + 特征头 X-LocalTrans-TestAPI
 *  16. 汇总表 + 清理进程（finally，taskkill 连进程树）
 *
 * Node 18+（全局 fetch），零依赖。
 */
import { spawn, spawnSync, execSync } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const REPO = path.resolve(__dirname, '..', '..')
const EXE = path.join(REPO, 'target', 'debug', 'localtrans.exe')
const REPORTS = path.join(__dirname, 'reports')
const VITE_PORT = 1420
const API_PORT = 39871
const TOKEN = 'smoketest'
const BASE = `http://127.0.0.1:${API_PORT}`
const MARKER_HEADER = 'x-localtrans-testapi'

const log = (...a) => console.log('[smoke-m4]', ...a)

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ---- 步骤级记录（汇总表用） ----
/** @type {Array<{name: string, ok: boolean, note: string}>} */
const steps = []
let currentStep = ''

function record(ok, note = '') {
  steps.push({ name: currentStep, ok, note })
  if (!ok) process.exitCode = 1
  return ok
}

/** 包一步：异常即记 FAIL 并继续（依赖缺失由步骤内自行跳过） */
async function step(name, fn) {
  currentStep = name
  log(`▶ ${name}`)
  try {
    await fn()
  } catch (e) {
    record(false, String(e?.message ?? e).slice(0, 300))
  }
}

function fail(msg, detail) {
  if (detail !== undefined) {
    console.error(`[smoke-m4]   详情: ${typeof detail === 'string' ? detail : JSON.stringify(detail).slice(0, 1500)}`)
  }
  throw new Error(msg)
}

// ---- 进程管理（抄 smoke-m2/m3 模式） ----

/** Windows 下按端口找监听 PID 并 taskkill /T（连子进程树） */
function freePort(port) {
  const out = spawnSync('netstat', ['-ano', '-p', 'tcp'], { encoding: 'utf8' })
  if (out.status !== 0 || !out.stdout) return
  for (const line of out.stdout.split('\n')) {
    const cols = line.trim().split(/\s+/)
    if (cols[0] === 'TCP' && cols[1] && cols[1].endsWith(`:${port}`) && cols[4]) {
      log(`端口 ${port} 被 PID ${cols[4]} 占用，清理`)
      spawnSync('taskkill', ['/F', '/T', '/PID', cols[4]], { stdio: 'ignore' })
    }
  }
}

/** 杀同名进程（遗留实例会因 single-instance 转发导致冒烟拿到旧窗口） */
function killImage(image) {
  const r = spawnSync('taskkill', ['/F', '/T', '/IM', image], { encoding: 'utf8' })
  if (r.status === 0) log(`已清理遗留进程 ${image}`)
}

function killTree(pid) {
  if (!pid) return
  spawnSync('taskkill', ['/F', '/T', '/PID', String(pid)], { stdio: 'ignore' })
}

// ---- HTTP 辅助（统一包络解析） ----

async function reqJson(url, { method = 'GET', token = TOKEN, payload } = {}) {
  const resp = await fetch(url, {
    method,
    headers: {
      ...(payload !== undefined ? { 'Content-Type': 'application/json' } : {}),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: payload !== undefined ? JSON.stringify(payload) : undefined,
    signal: AbortSignal.timeout(30_000),
  })
  const text = await resp.text()
  let body = null
  try {
    body = JSON.parse(text)
  } catch {
    /* 非 JSON 体（如 PNG）原样留在 text */
  }
  return { resp, body, text }
}

const getJson = (url, opts) => reqJson(url, { ...opts, method: 'GET' })
const postJson = (url, payload, opts) => reqJson(url, { ...opts, method: 'POST', payload })

/** 断言包络 ok:true，失败抛错（带完整响应体） */
function expectOk({ resp, body, text }, what) {
  if (!resp.ok || !body || body.ok !== true) {
    fail(`${what} 失败：期望包络 ok:true`, `HTTP ${resp.status} ${String(text).slice(0, 2000)}`)
  }
  return body
}

/** 轮询直到 fetch 成功（任意状态码都算"端口活了"） */
async function waitHttpOk(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs
  let lastErr
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(url, { signal: AbortSignal.timeout(2000) })
      if (resp.ok) return
      lastErr = new Error(`status ${resp.status}`)
    } catch (e) {
      lastErr = e
    }
    await sleep(500)
  }
  throw new Error(`等待 ${url} 可访问超时（${timeoutMs}ms）：${lastErr}`)
}

// ---- 主流程 ----

async function main() {
  // ---- 1. 清理现场（保证脚本可重复运行） ----
  log('清理遗留进程与端口')
  killImage('localtrans.exe')
  freePort(VITE_PORT)
  freePort(API_PORT)
  await sleep(1000)

  const ctx = { runId: null, reportDir: null }
  let viteProc = null
  let appProc = null
  try {
    // ---- 2. vite dev --mode test-api ----
    log('启动 vite dev --mode test-api（1420）…')
    const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm'
    viteProc = spawn(npm, ['--prefix', path.join(REPO, 'ui'), 'run', 'dev:test'], {
      shell: true,
      stdio: 'ignore',
    })
    await waitHttpOk(`http://localhost:${VITE_PORT}/`, 60000)
    log('vite dev(test-api) 已就绪')

    // ---- 3. 构建 test-api debug exe（增量） ----
    log('cargo build -p localtrans --features test-api …')
    execSync('cargo build -p localtrans --features test-api', {
      cwd: REPO,
      stdio: 'inherit',
    })
    if (!existsSync(EXE)) fail(`产物不存在: ${EXE}`)

    // ---- 4. 启动应用（窗口会短暂弹出） ----
    log('启动 localtrans.exe（test-api: 39871）…')
    appProc = spawn(EXE, [], {
      cwd: path.dirname(EXE),
      env: {
        ...process.env,
        LOCALTRANS_TEST_API: '1',
        LOCALTRANS_TEST_API_KEY: TOKEN,
        LOCALTRANS_TEST_API_PORT: String(API_PORT),
      },
      stdio: 'ignore',
    })
    let appExitedEarly = false
    let finished = false
    appProc.on('exit', (code) => {
      if (!finished) {
        appExitedEarly = true
        log(`警告：应用进程提前退出，code=${code}`)
      }
    })

    // ---- 5. 等 health 200 且 bridgeReady=true ----
    log('等待 /api/health 且 bridgeReady=true（最多 45s）…')
    const deadline = Date.now() + 45_000
    let health = null
    for (;;) {
      if (appExitedEarly) fail('应用进程提前退出（构建产物或启动环境异常）')
      try {
        const r = await getJson(`${BASE}/api/health`)
        if (r.body?.data?.bridgeReady === true) {
          health = r
          break
        }
      } catch (e) {
        if (Date.now() > deadline) fail(`/api/health 等待超时: ${e}`)
      }
      if (Date.now() > deadline) {
        fail('45s 内 bridgeReady 未置位', health?.text ?? '（无 health 响应）')
      }
      await sleep(500)
    }
    log(`bridgeReady=true: ${health.text}`)

    // ================= 步骤矩阵 =================

    await step('health 包络与特征头', async () => {
      const r = await getJson(`${BASE}/api/health`)
      const body = expectOk(r, 'health')
      if (body?.data?.status !== 'ok') fail('health.data.status 应为 ok', r.text)
      const marker = r.resp.headers.get(MARKER_HEADER)
      if (marker !== '1') fail(`特征头 ${MARKER_HEADER} 应为 1`, `实际: ${marker}`)
      record(true, `marker=1`)
    })

    await step('version 版本握手', async () => {
      const r = await getJson(`${BASE}/api/version`)
      const body = expectOk(r, 'version')
      const d = body?.data ?? {}
      if (!/^\d+\.\d+\.\d+/.test(String(d.appVersion ?? ''))) {
        fail('appVersion 应为语义化版本', r.text)
      }
      if (d.apiVersion !== 1) fail('apiVersion 应为 1', r.text)
      if (d.bridgeReady !== true) fail('bridgeReady 应为 true', r.text)
      if (!['debug', 'release'].includes(d.buildProfile)) fail('buildProfile 非法', r.text)
      record(true, `appVersion=${d.appVersion} apiVersion=1 buildProfile=${d.buildProfile}`)
    })

    await step('test/begin 生成 runId', async () => {
      const r = await postJson(`${BASE}/api/test/begin`, { scenario: 'smoke-m4' })
      const body = expectOk(r, 'test/begin')
      const runId = body?.data?.runId
      if (typeof runId !== 'string' || !/^[0-9a-f]{32}$/.test(runId)) {
        fail('runId 应为 32 位小写 hex', r.text)
      }
      ctx.runId = runId
      record(true, `runId=${runId.slice(0, 8)}…`)
    })

    await step('test/step 打步骤标记', async () => {
      if (!ctx.runId) return record(false, '前置失败：无 runId')
      const r = await postJson(`${BASE}/api/test/step`, { name: 'navigate-settings' })
      expectOk(r, 'test/step')
      record(true, 'name=navigate-settings')
    })

    await step('ui/navigate → /settings', async () => {
      const r = await postJson(`${BASE}/api/ui/navigate`, { path: '/settings' })
      const body = expectOk(r, 'ui/navigate')
      if (body?.data?.path !== '/settings') fail('回传 path 应为 /settings', r.text)
      record(true, 'path=/settings')
    })

    await step('ui/wait 文本锚点"共享区"', async () => {
      const r = await postJson(`${BASE}/api/ui/wait`, { text: '共享区', timeoutMs: 8000 })
      expectOk(r, 'ui/wait 共享区')
      record(true, '设置页稳定锚点命中')
    })

    await step('ui/click 无害控件 nav-transfers-link', async () => {
      const r = await postJson(`${BASE}/api/ui/click`, { selector: '[testid=nav-transfers-link]' })
      const body = expectOk(r, 'ui/click')
      if (body?.data?.text !== '传输') fail('click 回传文本应为"传输"', r.text)
      record(true, 'text=传输（仅路由跳转，无副作用）')
    })

    await step('state/transfers TestSnapshot', async () => {
      const r = await getJson(`${BASE}/api/state/transfers`)
      const body = expectOk(r, 'state/transfers')
      const snap = body?.data ?? {}
      if (snap.schemaVersion !== 1) fail('schemaVersion 应为 1', r.text)
      if (!Array.isArray(snap.devices) || !Array.isArray(snap.transfers)) {
        fail('devices/transfers 应为数组', r.text)
      }
      record(true, `devices=${snap.devices?.length} transfers=${snap.transfers?.length}`)
    })

    await step('state/wait 永真条件首拍即中', async () => {
      const r = await postJson(`${BASE}/api/state/wait`, {
        source: 'transfers', path: 'devices.length', op: 'gte', value: 0,
      })
      const body = expectOk(r, 'state/wait')
      if (body?.data?.matched !== true || body?.data?.polls !== 1) {
        fail('matched 应为 true 且 polls=1', r.text)
      }
      record(true, `observed=${body?.data?.observed}`)
    })

    await step('screenshot PNG + X-* 头 + 落盘', async () => {
      const resp = await fetch(`${BASE}/api/screenshot`, {
        headers: { Authorization: `Bearer ${TOKEN}` },
        signal: AbortSignal.timeout(30_000),
      })
      const buf = Buffer.from(await resp.arrayBuffer())
      if (resp.status !== 200) fail(`截图应 200，实际 ${resp.status}`, buf.toString('utf8').slice(0, 500))
      // PNG 魔数：89 50 4E 47 0D 0A 1A 0A
      const magic = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
      if (buf.length < 8 || magic.some((b, i) => buf[i] !== b)) {
        fail('响应体应为 PNG（魔数字节不符）', `前 8 字节: ${buf.subarray(0, 8).toString('hex')}`)
      }
      const ctype = resp.headers.get('content-type')
      if (ctype !== 'image/png') fail(`Content-Type 应为 image/png，实际 ${ctype}`)
      const w = Number(resp.headers.get('x-width'))
      const h = Number(resp.headers.get('x-height'))
      const scale = Number(resp.headers.get('x-scale'))
      if (!(w > 0 && h > 0)) fail(`X-Width/X-Height 应为正数`, `w=${w} h=${h}`)
      if (!(scale > 0)) fail(`X-Scale 应为正数（DPI 缩放）`, `scale=${resp.headers.get('x-scale')}`)
      if (buf.length <= 10 * 1024) fail(`PNG 应 >10KB，实际 ${buf.length}B`)
      // 落盘工件：m4-<YYYYMMDD-HHMMSS>
      const d = new Date()
      const p2 = (n) => String(n).padStart(2, '0')
      const ts = `${d.getFullYear()}${p2(d.getMonth() + 1)}${p2(d.getDate())}-${p2(d.getHours())}${p2(d.getMinutes())}${p2(d.getSeconds())}`
      ctx.reportDir = path.join(REPORTS, `m4-${ts}`)
      mkdirSync(ctx.reportDir, { recursive: true })
      const file = path.join(ctx.reportDir, 'shot-1.png')
      writeFileSync(file, buf)
      record(true, `${w}x${h}@${scale}x ${buf.length}B → ${path.relative(REPO, file)}`)
    })

    await step('logs/tail 按 runId 过滤出标记', async () => {
      if (!ctx.runId) return record(false, '前置失败：无 runId')
      const r = await getJson(`${BASE}/api/logs/tail?runId=${ctx.runId}`)
      const body = expectOk(r, 'logs/tail')
      const entries = body?.data?.entries ?? []
      if (entries.length === 0) fail('按 runId 过滤应至少命中 begin/step 标记', r.text)
      const msgs = entries.map((e) => e.message).join('\n')
      if (!msgs.includes('test run 开始')) fail('应含 begin 标记（test run 开始）', msgs.slice(0, 800))
      if (!msgs.includes('test step')) fail('应含 step 标记', msgs.slice(0, 800))
      if (!entries.every((e) => e.runId === ctx.runId)) fail('过滤结果 runId 应全等于请求值', r.text.slice(0, 800))
      // step 字段：begin 之后的条目应带 step（step 标记本身与后续日志）
      const withStep = entries.filter((e) => typeof e.step === 'string' && e.step.length > 0)
      if (withStep.length === 0) fail('应存在带 step 字段的条目（step 标记落日志流）', msgs.slice(0, 800))
      record(true, `entries=${entries.length}（含开始/步骤标记，step 关联生效）`)
    })

    await step('invoke 白名单内 get_settings → 200', async () => {
      const r = await postJson(`${BASE}/api/invoke`, { cmd: 'get_settings', args: {} })
      const body = expectOk(r, 'invoke get_settings')
      const d = body?.data
      if (typeof d !== 'object' || d === null) fail('data 应为设置对象', r.text)
      // ConfigDto 至少带设备名类字段（蛇形口径与 commands.rs 一致）
      const keys = Object.keys(d)
      if (keys.length === 0) fail('设置对象不应为空', r.text)
      record(true, `data 字段数=${keys.length}`)
    })

    await step('invoke 白名单外 remove_transfer → 403', async () => {
      const r = await postJson(`${BASE}/api/invoke`, {
        cmd: 'remove_transfer', args: { job_id: '0000000000000001', level: 'destroy' },
      })
      if (r.resp.status !== 403 || r.body?.error?.code !== 'INVOKE_NOT_ALLOWED') {
        fail('破坏性命令应得 403 INVOKE_NOT_ALLOWED', r.text)
      }
      const allowed = r.body?.error?.detail?.allowed
      if (!Array.isArray(allowed) || allowed.length === 0) {
        fail('detail.allowed 应为非空数组', r.text)
      }
      const hasClass = allowed.every((a) => a?.class === 'ReadOnly' || a?.class === 'Mutating')
      if (!hasClass) fail('allowed[].class 应为 ReadOnly/Mutating', r.text)
      record(true, `allowed=${allowed.length} 项（含 Class）`)
    })

    await step('test/end 收尾与统计', async () => {
      if (!ctx.runId) return record(false, '前置失败：无 runId')
      const r = await postJson(`${BASE}/api/test/end`, { runId: ctx.runId, outcome: 'pass' })
      const body = expectOk(r, 'test/end')
      if (body?.data?.runId !== ctx.runId) fail('回传 runId 应一致', r.text)
      if (body?.data?.outcome !== 'pass') fail('回传 outcome 应为 pass', r.text)
      const entries = body?.data?.entries
      if (typeof entries !== 'number' || entries < 3) {
        fail('entries 统计应 ≥3（至少 begin/step/end 三个标记）', r.text)
      }
      record(true, `entries=${entries}`)
      ctx.runId = null
    })

    await step('test/end 负路径：无活动 run → 400', async () => {
      const r = await postJson(`${BASE}/api/test/end`, {
        runId: '0'.repeat(32), outcome: 'pass',
      })
      if (r.resp.status !== 400 || r.body?.error?.code !== 'BAD_REQUEST') {
        fail('无活动 run 的 end 应得 400 BAD_REQUEST', r.text)
      }
      record(true, '400 BAD_REQUEST')
    })

    await step('test/step 负路径：无活动 run → 400', async () => {
      const r = await postJson(`${BASE}/api/test/step`, { name: 'orphan' })
      if (r.resp.status !== 400 || r.body?.error?.code !== 'BAD_REQUEST') {
        fail('无活动 run 的 step 应得 400 BAD_REQUEST', r.text)
      }
      record(true, '400 BAD_REQUEST')
    })

    await step('fallback：未知路径 404 NOT_FOUND 包络', async () => {
      const r = await getJson(`${BASE}/api/no/such/endpoint`)
      if (r.resp.status !== 404 || r.body?.error?.code !== 'NOT_FOUND') {
        fail('未知路径应得 404 NOT_FOUND 包络', r.text)
      }
      const marker = r.resp.headers.get(MARKER_HEADER)
      if (marker !== '1') fail('404 响应也应带特征头', `实际: ${marker}`)
      record(true, '404 包络 + 特征头')
    })

    // ---- 汇总 ----
    const width = Math.max(...steps.map((s) => s.name.length)) + 2
    console.log('\n[smoke-m4] 步骤汇总：')
    for (const s of steps) {
      console.log(`  ${(s.ok ? 'PASS' : 'FAIL').padEnd(5)} ${s.name.padEnd(width)} ${s.note}`)
    }
    const failed = steps.filter((s) => !s.ok).length
    console.log(`\n[smoke-m4] ${steps.length - failed}/${steps.length} 步骤通过` + (failed ? `（失败 ${failed}）` : '（全部通过）'))
    if (failed) process.exitCode = 1
    finished = true // 收尾标记：finally 的 taskkill 引发的退出不算"提前退出"
  } finally {
    // ---- 清理（无论成败） ----
    log('清理进程')
    killTree(appProc?.pid)
    killTree(viteProc?.pid)
    freePort(API_PORT)
    freePort(VITE_PORT)
  }
}

main().catch((e) => {
  console.error('[smoke-m4] FAIL（未预期）:', e)
  process.exitCode = 1
  process.exit(process.exitCode ?? 1)
})
