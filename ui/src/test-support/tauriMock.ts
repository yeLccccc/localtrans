/**
 * 浏览器预览用的 Tauri API mock(design-review 专用)。
 * 仅当不在 Tauri WebView 内时激活,提供演示数据让 UI 可以脱离 Rust 后端走查。
 * 生产构建不受影响:Vite 静态 import + 运行时判断。
 */

type Listener = (payload: unknown) => void

const listeners = new Map<string, Set<Listener>>()

function emitMock(event: string, payload: unknown) {
  listeners.get(event)?.forEach((fn) => fn(payload))
}
void emitMock

/** 演示用设备列表:一台未信任 + 一台已信任 */
const mockDevices = [
  {
    fingerprint: 'a3f291bc47de55aa1234beef567890ab',
    name: '书房-台式机',
    addr: '192.168.1.23:47601',
    quic_port: 47601,
    last_seen_ms: Date.now(),
    trusted: false,
    source: 'lan',
  },
  {
    fingerprint: '7d22e9f01a4c88bb3345ccdd6677ee11',
    name: '客厅-笔记本',
    addr: '192.168.1.42:47601',
    quic_port: 47601,
    last_seen_ms: Date.now(),
    trusted: true,
    source: 'lan',
  },
]

/** 演示用传输任务:三角色各一条 */
const mockTransfers = [
  {
    job_id: '0000000000000001',
    name: '项目备份_2026-08-20.zip',
    total: 2_147_483_648,
    done: 1_288_490_188,
    state: 'active',
    speed_bps: 22 * 1024 * 1024,
    peer: '客厅-笔记本',
    direction: 'pull',
    local_role: 'destination',
    health: { loss_ratio: 0.004, rtt_ms: 3, cwnd: 480, streams: 6 },
    started_at_ms: Date.now() - 42_000,
  },
  {
    job_id: '8000000000000001',
    name: '假期照片合集.tar',
    total: 5_368_709_120,
    done: 858_993_459,
    state: 'active',
    speed_bps: 14 * 1024 * 1024,
    peer: '书房-台式机',
    direction: 'push',
    local_role: 'source-push',
    health: { loss_ratio: 0.012, rtt_ms: 8, cwnd: 220, streams: 4 },
    started_at_ms: Date.now() - 120_000,
  },
  {
    job_id: '8000000000000002',
    name: '设计稿_v12.fig',
    total: 483_183_820,
    done: 402_653_184,
    state: 'active',
    speed_bps: 8 * 1024 * 1024,
    peer: '客厅-笔记本',
    direction: 'pull',
    local_role: 'source-pull',
    health: { loss_ratio: 0.002, rtt_ms: 2, cwnd: 640, streams: 8 },
    started_at_ms: Date.now() - 65_000,
  },
]

const mockConfig = {
  device_name: '我的电脑',
  download_dir: 'D:\\Downloads\\LocalTrans',
  hidden: false,
  shares: [
    { id: 'share1', alias: '工作文档', path: 'D:\\Work\\Docs' },
    { id: 'share2', alias: '电影', path: 'E:\\Media\\Movies' },
  ],
}

function invokeMock(command: string, _args?: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'list_devices':
      return Promise.resolve(mockDevices.map((d, i) => ({ ...d, online: true, connected: i === 1 })))
    case 'list_transfers':
      return Promise.resolve(mockTransfers)
    case 'get_settings':
      return Promise.resolve(mockConfig)
    case 'list_shares_remote':
      return Promise.resolve([
        { id: 'share1', alias: '工作文档' },
        { id: 'share2', alias: '电影' },
      ])
    case 'list_dir_remote':
      return Promise.resolve({
        entries: [
          { name: '年度报告.docx', is_dir: false, size: 2_411_724, mtime: 1754800000 },
          { name: '产品规划', is_dir: true, size: 0, mtime: 1754700000 },
          { name: '会议纪要', is_dir: true, size: 0, mtime: 1754600000 },
          { name: '架构设计 v3.pdf', is_dir: false, size: 89_133_056, mtime: 1754500000 },
          { name: '预算表.xlsx', is_dir: false, size: 156_672, mtime: 1754400000 },
        ],
        next_cursor: null,
      })
    case 'list_trusted':
      return Promise.resolve([
        {
          fingerprint: '7d22e9f01a4c88bb3345ccdd6677ee11',
          name: '客厅-笔记本',
          paired_at: 1754000000,
          perms: { browse: true, download: true, push: 'ask' },
        },
      ])
    case 'pending_resume_jobs':
      return Promise.resolve([])
    case 'get_device_fingerprint':
      return Promise.resolve({ fingerprint: 'c1d2e3f405a6b7c8d9e0f1a2b3c4d5e6', short_code: 'C1D2-E3F4' })
    case 'get_network_status':
      return Promise.resolve({
        ip: '192.168.1.10',
        firewall: { rule_exists: true, rule_enabled: true },
      })
    case 'list_channels':
      // M3c T1 演示数据:台式机直连 2ms,笔记本经中继 120ms(设备卡通道标签三态)
      return Promise.resolve([
        {
          fingerprint: 'a3f291bc47de55aa1234beef567890ab',
          addr: '192.168.1.23:47601',
          via_relay: false,
          rtt_ms: 2,
          est_bps: 90_000_000,
          loss_rate: 0,
          current: true,
          score_ready: true,
          probe_disabled: false,
          age_secs: 5,
        },
        {
          fingerprint: '7d22e9f01a4c88bb3345ccdd6677ee11',
          addr: '10.8.0.17:51022',
          via_relay: true,
          rtt_ms: 120,
          est_bps: 8_000_000,
          loss_rate: 0.1,
          current: true,
          score_ready: true,
          probe_disabled: false,
          age_secs: 42,
        },
      ])
    case 'probe_now_peer':
      // M3c T2 手动单对端快检:mock 下无真实探测,成功空值即可
      return Promise.resolve(null)
    default:
      // 其余 command 一律返回"成功空值",让页面能渲染
      return Promise.resolve(null)
  }
}

export function installTauriMockIfBrowser() {
  // @ts-expect-error 探测 Tauri 内部注入对象
  if (typeof window !== 'undefined' && window.__TAURI_INTERNALS__) {
    return // 真正的 Tauri 环境,不干预
  }
  if (typeof window === 'undefined') {
    return
  }

  // @ts-expect-error 运行时补丁
  window.__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
    transformCallback: (cb: (payload: unknown) => void) => {
      const id = Math.floor(Math.random() * 1e9)
      // @ts-expect-error 注册到全局回调表
      ;(window[`_${id}`] as unknown) = cb
      return id
    },
    metadata: {
      currentWindow: { label: 'main' },
      currentWebview: { label: 'main' },
    },
    // Task M1: 浏览器 mock 标记——logBridge 据此与真实 Tauri 区分，
    // 浏览器预览下日志桥保持 no-op
    __localtransMock: true,
  }
  // @ts-expect-error 事件通道补丁
  window.__TAURI_EVENT_PLUGIN__ = true
}
