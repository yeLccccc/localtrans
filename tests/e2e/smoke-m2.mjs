#!/usr/bin/env node
/**
 * Task M2 冒烟：UI 通道（testBridge 往返 + /api/ui/* 端点）端到端验证
 *（spec §7.1.4，plan docs/plans/0001-e2e-test-automation.md Task M2）
 *
 * 流程：
 *   1. 清理：杀遗留 localtrans.exe、释放 1420/39871 端口（可重复运行）
 *   2. 后台起 vite dev --mode test-api（ui 1420，前端带 __testBridge）
 *   3. cargo build -p localtrans --features test-api（增量）
 *   4. 设 LOCALTRANS_TEST_API=1 KEY=smoketest PORT=39871 启动 debug exe
 *   5. 等 /api/health 200 且 data.bridgeReady === true（test_bridge_hello 握手）
 *   6. 负路径：未认证 GET /api/ui/tree → 401
 *   7. ui/navigate {"path":"/settings"} → ui/wait 文本"共享区"（设置页稳定锚点，
 *      规避底部导航"设置"标签造成的假命中）
 *   8. ui/tree：断言含 ≥3 个带 testId 的元素（nav-* + settings-*）
 *   9. ui/click 一个无害按钮：nav-transfers-link（仅路由跳转，无副作用）→
 *      ui/wait 文本"传输任务"（确认跳转真实生效）→ 再 navigate 回 /settings
 *  10. 清理：taskkill 杀 exe 与 vite 进程树，兜底再释放端口
 *
 * Node 18+（全局 fetch），零依赖。任何一步失败打印完整响应体并退出码 1。
 * 注意：会短暂弹出应用窗口，结束自动清理。
 */
import { spawn, spawnSync, execSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const REPO = path.resolve(__dirname, '..', '..')
const EXE = path.join(REPO, 'target', 'debug', 'localtrans.exe')
const VITE_PORT = 1420
const API_PORT = 39871
const TOKEN = 'smoketest'
const BASE = `http://127.0.0.1:${API_PORT}`

const log = (...a) => console.log('[smoke-m2]', ...a)

function diag(msg, detail) {
  console.error('[smoke-m2] FAIL:', msg)
  if (detail !== undefined) {
    console.error('[smoke-m2] 响应体/详情:', typeof detail === 'string' ? detail : JSON.stringify(detail, null, 2))
  }
  process.exitCode = 1
  throw new Error(String(msg))
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

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

/** GET 并解析统一包络；非 2xx 抛错（带响应体） */
async function getJson(url, token) {
  const headers = token ? { Authorization: `Bearer ${token}` } : {}
  const resp = await fetch(url, { headers, signal: AbortSignal.timeout(30_000) })
  const text = await resp.text()
  let body = null
  try {
    body = JSON.parse(text)
  } catch {
    /* 非 JSON 体原样保留在 text */
  }
  return { resp, body, text }
}

/** POST JSON 并解析统一包络 */
async function postJson(url, token, payload) {
  const resp = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(30_000),
  })
  const text = await resp.text()
  let body = null
  try {
    body = JSON.parse(text)
  } catch {
    /* 非 JSON 体原样保留在 text */
  }
  return { resp, body, text }
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
  diag(`等待 ${url} 可访问超时（${timeoutMs}ms）：${lastErr}`)
}

/** 断言包络 ok:true，失败打印完整响应体 */
function expectOk(step, { resp, body, text }) {
  if (!resp.ok || !body || body.ok !== true) {
    diag(`${step} 失败：期望包络 ok:true`, `HTTP ${resp.status} ${text.slice(0, 2000)}`)
  }
  log(`${step} OK`)
  return body
}

async function main() {
  // ---- 1. 清理现场（保证脚本可重复运行） ----
  log('清理遗留进程与端口')
  killImage('localtrans.exe')
  freePort(VITE_PORT)
  freePort(API_PORT)
  await sleep(1000)

  let viteProc = null
  let appProc = null
  let finished = false
  try {
    // ---- 2. vite dev --mode test-api（前端注册 window.__testBridge） ----
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
    if (!existsSync(EXE)) diag(`产物不存在: ${EXE}`)

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
    appProc.on('exit', (code) => {
      if (!finished) log(`警告：应用进程提前退出，code=${code}`)
    })

    // ---- 5. 等 health 200 且 bridgeReady=true（前端 hello 握手，最多 45s） ----
    log('等待 /api/health 且 bridgeReady=true（前端 testBridge 握手，最多 45s）…')
    const deadline = Date.now() + 45_000
    let health = null
    for (;;) {
      try {
        const r = await getJson(`${BASE}/api/health`)
        if (r.body?.data?.bridgeReady === true) {
          health = r
          break
        }
        if (r.body?.data?.status !== 'ok') {
          diag('health 包络异常', r.text)
        }
      } catch (e) {
        if (Date.now() > deadline) diag(`/api/health 等待超时: ${e}`)
      }
      if (Date.now() > deadline) {
        diag('45s 内 bridgeReady 未置位（前端未以 --mode test-api 构建或 hello 未达）', health?.text ?? '（无 health 响应）')
      }
      await sleep(500)
    }
    log(`bridgeReady=true（握手完成）: ${health.text}`)

    // ---- 6. 负路径：未认证 ui/tree → 401 ----
    {
      const r = await getJson(`${BASE}/api/ui/tree`)
      if (r.resp.status !== 401 || r.body?.error?.code !== 'AUTH_FAILED') {
        diag('未认证 ui/tree 应得 401 AUTH_FAILED', r.text)
      }
      log('未认证 ui/tree → 401 OK')
    }

    // ---- 7. navigate /settings + wait 设置页锚点 ----
    expectOk('ui/navigate /settings', await postJson(`${BASE}/api/ui/navigate`, TOKEN, { path: '/settings' }))
    expectOk(
      'ui/wait text=共享区（设置页稳定锚点）',
      await postJson(`${BASE}/api/ui/wait`, TOKEN, { text: '共享区', timeoutMs: 8000 }),
    )

    // ---- 8. tree：≥3 个带 testId 的元素，且含 nav-settings-link ----
    {
      const r = await getJson(`${BASE}/api/ui/tree`, TOKEN)
      const body = expectOk('ui/tree', r)
      const elements = body?.data ?? []
      if (!Array.isArray(elements)) diag('ui/tree data 应为数组', r.text)
      const withTestId = elements.filter((e) => e && typeof e.testId === 'string' && e.testId.length > 0)
      if (withTestId.length < 3) {
        diag(`ui/tree 带 testId 的元素应 ≥3，实际 ${withTestId.length}`, r.text)
      }
      if (!withTestId.some((e) => e.testId === 'nav-settings-link')) {
        diag('ui/tree 应包含 nav-settings-link（fixed 元素不得被剪枝）', r.text)
      }
      log(`ui/tree OK：${elements.length} 个可交互元素，其中 ${withTestId.length} 个带 testId`)
    }

    // ---- 9. 无害 click：底部导航 → /transfers，确认跳转生效后回到 /settings ----
    {
      const r = await postJson(`${BASE}/api/ui/click`, TOKEN, { selector: '[testid=nav-transfers-link]' })
      const body = expectOk('ui/click nav-transfers-link', r)
      if (body?.data?.text !== '传输') {
        diag('click 应回传元素文本"传输"', r.text)
      }
      expectOk(
        'ui/wait text=传输任务（确认跳转生效）',
        await postJson(`${BASE}/api/ui/wait`, TOKEN, { text: '传输任务', timeoutMs: 8000 }),
      )
      expectOk('ui/navigate 回 /settings（恢复现场）', await postJson(`${BASE}/api/ui/navigate`, TOKEN, { path: '/settings' }))
    }

    log('PASS：testBridge 往返 + ui/* 端点端到端链路全部打通')
    finished = true
  } finally {
    // ---- 10. 清理（无论成败） ----
    log('清理进程')
    killTree(appProc?.pid)
    killTree(viteProc?.pid)
    freePort(API_PORT)
    freePort(VITE_PORT)
  }
}

main().catch((e) => {
  if (process.exitCode !== 1) {
    console.error('[smoke-m2] FAIL（未预期）:', e)
    process.exitCode = 1
  }
  process.exit(process.exitCode ?? 1)
})
