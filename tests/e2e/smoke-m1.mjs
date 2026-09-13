#!/usr/bin/env node
/**
 * Task M1 冒烟：环形缓冲 + /api/logs/tail + 前端 console 桥 端到端验证
 *（spec §7.1.7，plan docs/plans/0001-e2e-test-automation.md Task M1）
 *
 * 流程：
 *   1. 清理：杀遗留 localtrans.exe、释放 1420/39871 端口（可重复运行）
 *   2. 后台起 vite dev（ui 1420 端口，debug 构建的 devUrl 指向它）
 *   3. cargo build -p localtrans --features test-api（增量，产物 target/debug/localtrans.exe）
 *   4. 设 LOCALTRANS_TEST_API=1 KEY=smoketest PORT=39871 启动 debug exe
 *   5. 等 /api/health 200（免认证）；顺带断言 logs/tail 缺 token 得 401
 *   6. 轮询 /api/logs/tail?target=ui&level=info 直到出现 "logBridge attached"
 *      （logBridge attach 成功即上报的锚点，经 console 桥 → ui_log → tracing → 环形缓冲）
 *   7. 断言 nextSeq 游标语义：第二次传 afterSeq=首次返回的 nextSeq，条目不重复
 *   8. 清理：taskkill 杀 exe 与 vite 进程树，兜底再释放端口
 *
 * Node 18+（用全局 fetch），零依赖。任何一步失败打印诊断并退出码 1。
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

const log = (...a) => console.log('[smoke-m1]', ...a)

function diag(msg) {
  console.error('[smoke-m1] FAIL:', msg)
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
    // TCP  127.0.0.1:1420  0.0.0.0:0  LISTENING  12345
    if (cols[0] === 'TCP' && cols[1] && cols[1].endsWith(`:${port}`) && cols[4]) {
      log(`端口 ${port} 被 PID ${cols[4]} 占用，清理`)
      spawnSync('taskkill', ['/F', '/T', '/PID', cols[4]], { stdio: 'ignore' })
    }
  }
}

/** 杀同名进程（遗留的本地实例会因 single-instance 插件转发导致冒烟拿到旧窗口） */
function killImage(image) {
  const r = spawnSync('taskkill', ['/F', '/T', '/IM', image], { encoding: 'utf8' })
  if (r.status === 0) log(`已清理遗留进程 ${image}`)
}

function killTree(pid) {
  if (!pid) return
  spawnSync('taskkill', ['/F', '/T', '/PID', String(pid)], { stdio: 'ignore' })
}

/** GET 并解析统一包络；非 2xx 时抛错（带响应体片段） */
async function getJson(url, token) {
  const headers = token ? { Authorization: `Bearer ${token}` } : {}
  const resp = await fetch(url, { headers, signal: AbortSignal.timeout(4000) })
  const text = await resp.text()
  if (!resp.ok) {
    throw new Error(`GET ${url} -> ${resp.status}: ${text.slice(0, 200)}`)
  }
  return { resp, body: JSON.parse(text) }
}

/** 轮询直到 fetch 成功（任意状态码都算"端口活了"，由调用方再校验内容） */
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

async function main() {
  // ---- 1. 清理现场（保证脚本可重复运行） ----
  log('清理遗留进程与端口')
  killImage('localtrans.exe')
  freePort(VITE_PORT)
  freePort(API_PORT)
  await sleep(1000)

  let viteProc = null
  let appProc = null
  let finished = false // 成功/失败判定完成后，进程退出（被 taskkill）不再是"提前退出"
  try {
    // ---- 2. vite dev（debug 构建走 devUrl localhost:1420，必须先起） ----
    log('启动 vite dev（1420）…')
    const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm'
    viteProc = spawn(npm, ['--prefix', path.join(REPO, 'ui'), 'run', 'dev'], {
      shell: true,
      stdio: 'ignore',
    })
    await waitHttpOk(`http://localhost:${VITE_PORT}/`, 60000)
    log('vite dev 已就绪')

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
      if (!finished) {
        // 尚未判定成功就退出 → 记录，主循环的等待会超时并给出诊断
        log(`警告：应用进程提前退出，code=${code}`)
      }
    })

    // ---- 5. /api/health 200（免认证）+ 401 门禁顺带断言 ----
    const deadline = Date.now() + 30000
    let health
    for (;;) {
      try {
        health = await getJson(`${BASE}/api/health`)
        break
      } catch (e) {
        if (Date.now() > deadline) diag(`/api/health 等待超时: ${e}`)
        await sleep(500)
      }
    }
    if (health.body?.ok !== true || health.body?.data?.status !== 'ok') {
      diag(`health 包络异常: ${JSON.stringify(health.body).slice(0, 200)}`)
    }
    if (health.resp.headers.get('x-localtrans-testapi') !== '1') {
      diag('响应缺少 X-LocalTrans-TestAPI: 1 特征头')
    }
    log('/api/health OK（bridgeReady=false 属 M1 预期，M2 才置位）')

    {
      let unauthorized = null
      try {
        await getJson(`${BASE}/api/logs/tail`)
      } catch (e) {
        unauthorized = e
      }
      if (!unauthorized || !unauthorized.message.includes('401')) {
        diag('logs/tail 缺 token 应返回 401')
      }
    }

    // ---- 6. 等 logBridge attached（target=ui，最多 30s） ----
    log('等待前端 logBridge 上报锚点（最多 30s）…')
    const anchorDeadline = Date.now() + 30000
    let first = null
    for (;;) {
      const { body } = await getJson(
        `${BASE}/api/logs/tail?target=ui&level=info`,
        TOKEN,
      )
      const entries = body?.data?.entries ?? []
      const anchor = entries.find((e) => (e.message ?? '').includes('logBridge attached'))
      if (anchor) {
        first = { body, anchor }
        break
      }
      if (Date.now() > anchorDeadline) {
        const { body: raw } = await getJson(`${BASE}/api/logs/tail`, TOKEN)
        const sample = (raw?.data?.entries ?? []).slice(-10).map((e) => `${e.seq} ${e.level} ${e.target} ${e.message}`)
        diag(
          `30s 内未见 target=ui 的 "logBridge attached"。最近日志：\n  ${sample.join('\n  ')}`,
        )
      }
      await sleep(500)
    }
    const anchorSeq = first.anchor.seq
    const nextSeq = first.body.data.nextSeq
    log(`锚点已出现（seq=${anchorSeq}, nextSeq=${nextSeq}），message="${first.anchor.message}"`)
    if (nextSeq < anchorSeq) diag(`nextSeq(${nextSeq}) 不应小于锚点 seq(${anchorSeq})`)

    // ---- 7. 游标语义：afterSeq=nextSeq 续读，不重复 ----
    const second = await getJson(`${BASE}/api/logs/tail?target=ui&afterSeq=${nextSeq}`, TOKEN)
    const secondEntries = second.body?.data?.entries ?? []
    const dup = secondEntries.filter((e) => e.seq <= nextSeq)
    if (dup.length > 0) diag(`续读出现 seq <= afterSeq 的重复条目: ${JSON.stringify(dup[0])}`)
    if (secondEntries.some((e) => e.seq === anchorSeq)) diag('锚点条目在续读中重复出现')
    log(`游标续读 OK：${secondEntries.length} 条新条目，全部 seq > ${nextSeq}`)

    log('PASS：环形缓冲/logs/tail/console 桥端到端链路全部打通')
    finished = true
  } finally {
    // ---- 8. 清理（无论成败） ----
    log('清理进程')
    killTree(appProc?.pid)
    killTree(viteProc?.pid)
    freePort(API_PORT)
    freePort(VITE_PORT)
  }
}

main().catch((e) => {
  // diag 已设置 exitCode；这里兜住未预期异常
  if (process.exitCode !== 1) {
    console.error('[smoke-m1] FAIL（未预期）:', e)
    process.exitCode = 1
  }
  process.exit(process.exitCode ?? 1)
})
