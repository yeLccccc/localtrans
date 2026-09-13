#!/usr/bin/env node
/**
 * Task M3 冒烟：状态断言（TestSnapshot + state 端点 + state/wait）端到端验证
 *（spec §7.1.5 / §7.1.6，plan docs/plans/0001-e2e-test-automation.md Task M3）
 *
 * 流程：
 *   1. 清理：杀遗留 localtrans.exe、释放 1420/39871 端口（可重复运行）
 *   2. 后台起 vite dev --mode test-api（ui 1420，前端 __testBridge 带 state 动作）
 *   3. cargo build -p localtrans --features test-api（增量）
 *   4. 设 LOCALTRANS_TEST_API=1 KEY=smoketest PORT=39871 启动 debug exe
 *   5. 等 /api/health 200 且 data.bridgeReady === true（state/app 依赖桥就绪）
 *   6. GET /api/state/transfers：断言 schemaVersion === 1 且 devices 是数组、
 *      selfDevice.id 为 64 位 hex（单机无对端 devices 可能空，属正常）
 *   7. GET /api/state/app：断言 devices/transfers/settings/toast 四 store 键在，
 *      且各内层 schemaVersion === 1
 *   8. POST /api/state/wait {source:"transfers", path:"devices.length", op:"gte",
 *      value:0} → 立即满足（data.matched === true 且 polls === 1）
 *   9. 负路径 a：op:"pfx" → 400 BAD_REQUEST
 *      负路径 b：source:"bogus" → 400 BAD_REQUEST
 *      负路径 c：永假条件 devices.length gte 999（timeoutMs 1500）→
 *      408 WAIT_TIMEOUT 且 detail.lastValue === 0（末次观测值）
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

const log = (...a) => console.log('[smoke-m3]', ...a)

function diag(msg, detail) {
  console.error('[smoke-m3] FAIL:', msg)
  if (detail !== undefined) {
    console.error('[smoke-m3] 响应体/详情:', typeof detail === 'string' ? detail : JSON.stringify(detail, null, 2))
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

/** GET 并解析统一包络 */
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
    // ---- 2. vite dev --mode test-api（前端 state 动作在 __testBridge 上） ----
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

    // ---- 5. 等 health 200 且 bridgeReady=true（state/app 走桥，必须先握手） ----
    log('等待 /api/health 且 bridgeReady=true（最多 45s）…')
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
    log(`bridgeReady=true: ${health.text}`)

    // ---- 6. state/transfers：TestSnapshot 形态 ----
    {
      const r = await getJson(`${BASE}/api/state/transfers`, TOKEN)
      const body = expectOk('state/transfers', r)
      const snap = body?.data ?? {}
      if (snap.schemaVersion !== 1) diag('state/transfers 应有 schemaVersion=1', r.text)
      if (!Array.isArray(snap.devices)) diag('devices 应为数组（合并视图）', r.text)
      if (!Array.isArray(snap.sessions)) diag('sessions 应为数组', r.text)
      if (!Array.isArray(snap.transfers)) diag('transfers 应为数组', r.text)
      if (!Array.isArray(snap.cards)) diag('cards 应为数组', r.text)
      if (snap.discoveryStats !== null) diag('discoveryStats 当前应恒 null（暂缺登记）', r.text)
      const id = snap.selfDevice?.id
      if (typeof id !== 'string' || !/^[0-9a-f]{64}$/.test(id)) {
        diag('selfDevice.id 应为 64 位 hex 指纹', r.text)
      }
      if (typeof snap.selfDevice?.name !== 'string' || snap.selfDevice.name.length === 0) {
        diag('selfDevice.name 应为非空设备名', r.text)
      }
      log(`state/transfers OK：devices=${snap.devices.length} sessions=${snap.sessions.length} transfers=${snap.transfers.length} cards=${snap.cards.length}`)
    }

    // ---- 7. state/app：四 store 聚合 ----
    {
      const r = await getJson(`${BASE}/api/state/app`, TOKEN)
      const body = expectOk('state/app', r)
      const snap = body?.data ?? {}
      if (snap.schemaVersion !== 1) diag('state/app 应有 schemaVersion=1', r.text)
      for (const key of ['devices', 'transfers', 'settings', 'toast']) {
        if (!snap[key] || typeof snap[key] !== 'object') {
          diag(`state/app 聚合应含 ${key} store 键`, r.text)
        }
        if (snap[key]?.schemaVersion !== 1) {
          diag(`state/app.${key}.schemaVersion 应为 1`, r.text)
        }
      }
      if (!Array.isArray(snap.devices?.devices)) diag('app.devices.devices 应为数组', r.text)
      if (!Array.isArray(snap.transfers?.transfers)) diag('app.transfers.transfers 应为数组', r.text)
      log('state/app OK：四 store 键齐全（devices/transfers/settings/toast）')
    }

    // ---- 8. state/wait 立即满足（首拍即中） ----
    {
      const r = await postJson(`${BASE}/api/state/wait`, TOKEN, {
        source: 'transfers', path: 'devices.length', op: 'gte', value: 0,
      })
      const body = expectOk('state/wait devices.length gte 0（立即满足）', r)
      if (body?.data?.matched !== true) diag('matched 应为 true', r.text)
      const observed = body?.data?.observed
      if (typeof observed !== 'number' || observed < 0) {
        diag('observed 应为 devices 数组长度（非负数）', r.text)
      }
      if (body?.data?.polls !== 1) diag('首拍即中 polls 应为 1', r.text)
    }

    // ---- 9a. 负路径：非法 op → 400 ----
    {
      const r = await postJson(`${BASE}/api/state/wait`, TOKEN, {
        source: 'transfers', path: 'devices.length', op: 'pfx', value: 0,
      })
      if (r.resp.status !== 400 || r.body?.error?.code !== 'BAD_REQUEST') {
        diag('非法 op 应得 400 BAD_REQUEST', r.text)
      }
      log('非法 op=pfx → 400 OK')
    }

    // ---- 9b. 负路径：非法 source → 400 ----
    {
      const r = await postJson(`${BASE}/api/state/wait`, TOKEN, {
        source: 'bogus', path: 'devices.length', op: 'eq', value: 1,
      })
      if (r.resp.status !== 400 || r.body?.error?.code !== 'BAD_REQUEST') {
        diag('非法 source 应得 400 BAD_REQUEST', r.text)
      }
      log('非法 source=bogus → 400 OK')
    }

    // ---- 9c. 负路径：永假条件超时 → 408 WAIT_TIMEOUT（detail 带末次观测值） ----
    {
      const started = Date.now()
      const r = await postJson(`${BASE}/api/state/wait`, TOKEN, {
        source: 'transfers', path: 'devices.length', op: 'gte', value: 999,
        timeoutMs: 1500, intervalMs: 500,
      })
      const elapsed = Date.now() - started
      if (r.resp.status !== 408 || r.body?.error?.code !== 'WAIT_TIMEOUT') {
        diag('永假条件超时应得 408 WAIT_TIMEOUT', r.text)
      }
      const detail = r.body?.error?.detail ?? {}
      if (typeof detail.lastValue !== 'number') {
        diag('408 detail 应带末次观测值 lastValue（devices.length 实测数）', r.text)
      }
      if (detail.path !== 'devices.length' || detail.op !== 'gte' || detail.value !== 999) {
        diag('408 detail 应回显条件（path/op/value）', r.text)
      }
      if (elapsed < 1400) diag(`超时必须等满 timeoutMs（实际 ${elapsed}ms）`, r.text)
      log(`永假条件 → 408 OK（${elapsed}ms，lastValue=${detail.lastValue}，polls=${detail.polls}）`)
    }

    log('PASS：TestSnapshot + state/app 聚合 + state/wait 满足/超时/负路径全部打通')
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
    console.error('[smoke-m3] FAIL（未预期）:', e)
    process.exitCode = 1
  }
  process.exit(process.exitCode ?? 1)
})
