/**
 * TransferItem 展示纯函数(传输域重构 Task 10)
 * 逻辑全部收拢在此,组件只做模板绑定。
 */

export interface PeerNameSource {
  fingerprint: string
  name: string
  alias: string
}

/** 指纹缩写:aa9988..ff12 */
export function fingerprintAbbr(fp: string): string {
  return fp.length > 12 ? `${fp.slice(0, 6)}...${fp.slice(-6)}` : fp
}

/**
 * D9:对端展示名 —— 别名 > 广播名 > 指纹缩写 aa9988..ff12
 * 指纹匹配不区分大小写(hex 展示口径不统一)
 */
export function peerDisplayName(peerHex: string, peers: PeerNameSource[]): string {
  const lower = peerHex.toLowerCase()
  const peer = peers.find((p) => p.fingerprint.toLowerCase() === lower)
  if (peer) {
    if (peer.alias) return peer.alias
    if (peer.name) return peer.name
  }
  return fingerprintAbbr(peerHex)
}

export interface ProgressPairInput {
  local_role?: string
  done: number
  remote_done?: number
  total: number
  state?: string
}

export interface ProgressPair {
  /** 主进度字节(发送方=remote_done,接收方=done) */
  mainDone: number
  /** 本机已发字节(发送方=done,接收方=done) */
  sentDone: number
  /** 网络积压百分比 (done-remote_done)/total*100,仅发送方有意义 */
  backlogPct: number
}

/**
 * D11 双进度:发送方(source-push)主进度=remote_done(对端已确认接收),
 * sentDone=本机 done;接收方主进度=done。
 *
 * 容错(旧任务无 remote_done 镜像):remote_done undefined 按 0;
 * 但若 done>0 且 remote_done===0 且 total>0 且非终态,视为"无镜像数据",
 * 退回单进度显示(mainDone=done),绝不显示 0% 误导。
 */
export function progressPair(t: ProgressPairInput): ProgressPair {
  const isSender = t.local_role === 'source-push'
  if (!isSender) {
    return { mainDone: t.done, sentDone: t.done, backlogPct: 0 }
  }
  const remote = t.remote_done ?? 0
  const terminal = ['done', 'failed', 'interrupted'].includes(t.state ?? '')
  const noMirrorData = t.done > 0 && remote === 0 && t.total > 0 && !terminal
  const mainDone = noMirrorData ? t.done : remote
  const backlogPct = t.total > 0 && !noMirrorData ? ((t.done - remote) / t.total) * 100 : 0
  return { mainDone, sentDone: t.done, backlogPct: Math.max(0, backlogPct) }
}

export type SenderDisplayState = 'normal' | 'backlog' | 'awaiting-confirm'

/**
 * 发送方展示态:
 * - awaiting-confirm:满格(done>=total>0)但对端未确认满(remote_done<total)
 * - backlog:积压 >20%
 * 优先级:满格未确认优先(此时积压必然大,但语义是"等对方收完")
 */
export function senderDisplayState(t: {
  done: number
  remote_done?: number
  total: number
  state: string
}): SenderDisplayState {
  const remote = t.remote_done ?? 0
  if (t.total > 0 && t.done >= t.total && remote < t.total) {
    return 'awaiting-confirm'
  }
  const pair = progressPair({
    local_role: 'source-push',
    done: t.done,
    remote_done: remote,
    total: t.total,
    state: t.state,
  })
  if (pair.backlogPct > 20) return 'backlog'
  return 'normal'
}

/**
 * 排队位次文案:queue_pos 非空 → "排队中 · 第 N 位";
 * 否则回退方向固定文案(pull=连对端调度,push=等人接收)
 */
export function queueText(queuePos: number | null | undefined, direction: string): string {
  if (queuePos !== null && queuePos !== undefined) {
    return `排队中 · 第 ${queuePos} 位`
  }
  return direction === 'pull' ? '排队中，连接对端...' : '等待对方接收...'
}

/** 子项重试请求参数形状(lastRequest 中记录的请求) */
export interface ChildRetryRequest {
  type: 'pull' | 'push' | 'push-rel'
  path?: string
  paths?: string[]
  items?: [string, string][]
}

/** 子项重试参数判定结果 */
export interface ChildRetryParams {
  path: string
  /** push-rel 时为对端落盘相对目录;push(绝对路径)时为空串 */
  relDir: string
}

/**
 * 从父卡记录的请求参数推导子项重试所需 (path, relDir)。
 * push-rel 需要 path+relDir;push(绝对路径)只需 path,relDir 传空串;
 * pull 需要 path。取不到返回 null(提示缺参)。
 */
export function childRetryParams(
  req: ChildRetryRequest | undefined | null,
  childPathHint?: string
): ChildRetryParams | null {
  if (!req) return null
  if (req.type === 'push-rel') {
    const item = req.items?.find(([, rel]) => rel)
    if (item) return { path: item[0], relDir: item[1] }
    return null
  }
  if (req.type === 'push') {
    const path = req.paths?.[0]
    return path ? { path, relDir: '' } : null
  }
  // pull
  const path = req.path || childPathHint
  return path ? { path, relDir: '' } : null
}

/** 分区所需最小字段(TransferDto 的结构子集) */
export interface TransferLike {
  job_id: string
  state: string
  queue_pos?: number | null
  started_at_ms?: number | null
  finished_at_ms?: number | null
}

const ACTIVE_STATES = new Set(['active', 'pending', 'paused', 'cancelling'])

/** 活动区状态权重:active > cancelling > pending > paused */
const ACTIVE_RANK: Record<string, number> = { active: 0, cancelling: 1, pending: 2, paused: 3 }

/**
 * 传输列表分区(传输域重构 Task 11):
 * active = active/pending/paused/cancelling,排序 active>cancelling>pending(queue_pos 升序,null 靠后)>paused,
 * 同级按 started_at_ms 升序(老的在前,null 靠后);
 * history = done/failed/interrupted,按 finished_at_ms 降序(新的在前,null 靠后)。
 */
export function splitActiveHistory<T extends TransferLike>(jobs: T[]): { active: T[]; history: T[] } {
  const active: T[] = []
  const history: T[] = []
  for (const j of jobs) {
    if (ACTIVE_STATES.has(j.state)) active.push(j)
    else history.push(j)
  }
  active.sort((a, b) => {
    const ra = ACTIVE_RANK[a.state] ?? 99
    const rb = ACTIVE_RANK[b.state] ?? 99
    if (ra !== rb) return ra - rb
    if (ra === 2) {
      // pending:queue_pos 升序,null 靠后
      const qa = a.queue_pos ?? null
      const qb = b.queue_pos ?? null
      if (qa !== null && qb !== null && qa !== qb) return qa - qb
      if (qa !== null) return -1
      if (qb !== null) return 1
    }
    const sa = a.started_at_ms ?? null
    const sb = b.started_at_ms ?? null
    if (sa !== null && sb !== null && sa !== sb) return sa - sb
    if (sa === null && sb !== null) return 1
    if (sa !== null && sb === null) return -1
    return 0
  })
  history.sort((a, b) => {
    const fa = a.finished_at_ms ?? null
    const fb = b.finished_at_ms ?? null
    if (fa !== null && fb !== null && fa !== fb) return fb - fa
    if (fa === null && fb !== null) return 1
    if (fa !== null && fb === null) return -1
    return 0
  })
  return { active, history }
}
