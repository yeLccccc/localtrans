import { describe, it, expect } from 'vitest'
import {
  peerDisplayName,
  progressPair,
  senderDisplayState,
  queueText,
  fingerprintAbbr,
  childRetryParams,
  splitActiveHistory,
} from '../transferDisplay'

describe('peerDisplayName', () => {
  const peers = [
    { fingerprint: 'AA9988CCDDEE1122334455667788990011223344', name: '我的手机', alias: '小米' },
    { fingerprint: 'FF11223344556677889900112233445566778899', name: '', alias: '' },
  ]

  it('别名优先', () => {
    expect(peerDisplayName('aa9988ccddee1122334455667788990011223344', peers)).toBe('小米')
  })
  it('无别名回退广播名', () => {
    const noAlias = [{ fingerprint: 'ff11223344556677889900112233445566778899', name: '华为平板', alias: '' }]
    expect(peerDisplayName('FF11223344556677889900112233445566778899', noAlias)).toBe('华为平板')
  })
  it('无信任记录回退指纹缩写', () => {
    expect(peerDisplayName('aabbccddeeff0011223344556677889900aabbcc', [])).toBe('aabbcc...aabbcc')
  })
  it('指纹缩写格式 aa9988..ccdd12 风格(前6...后6)', () => {
    const fp = 'aa99881122334455667788990011223344ccdd12'
    expect(peerDisplayName(fp, [])).toBe('aa9988...ccdd12')
  })
  it('短指纹原样返回', () => {
    expect(peerDisplayName('abc', [])).toBe('abc')
  })
  it('fingerprintAbbr 短指纹原样', () => {
    expect(fingerprintAbbr('abcd')).toBe('abcd')
  })
})

describe('progressPair', () => {
  it('source-push 主进度=remote_done, sentDone=done, 积压40%', () => {
    expect(
      progressPair({ local_role: 'source-push', done: 100, remote_done: 60, total: 100 })
    ).toEqual({ mainDone: 60, sentDone: 100, backlogPct: 40 })
  })
  it('接收方向 mainDone=done', () => {
    expect(
      progressPair({ local_role: 'destination', done: 70, remote_done: 999, total: 100 })
    ).toEqual({ mainDone: 70, sentDone: 70, backlogPct: 0 })
  })
  it('remote_done undefined 当 0(新发送任务)', () => {
    const r = progressPair({ local_role: 'source-push', done: 0, total: 100 })
    expect(r.mainDone).toBe(0)
    expect(r.backlogPct).toBe(0)
  })
  it('容错:done>0 且 remote_done=0 且 total>0 且非终态 → 视为无镜像数据退回单进度', () => {
    const r = progressPair({ local_role: 'source-push', done: 50, total: 100, state: 'active' })
    expect(r.mainDone).toBe(50)
    expect(r.backlogPct).toBe(0)
  })
  it('终态不触发无镜像容错(终态 remote_done=0 即真 0)', () => {
    const r = progressPair({ local_role: 'source-push', done: 50, total: 100, state: 'done' })
    expect(r.mainDone).toBe(0)
  })
})

describe('senderDisplayState', () => {
  it('满格未确认 → awaiting-confirm', () => {
    expect(senderDisplayState({ done: 100, remote_done: 80, total: 100, state: 'active' })).toBe('awaiting-confirm')
  })
  it('积压超 20% → backlog', () => {
    expect(senderDisplayState({ done: 40, remote_done: 10, total: 100, state: 'active' })).toBe('backlog')
  })
  it('积压恰 20% 不算 backlog', () => {
    expect(senderDisplayState({ done: 30, remote_done: 10, total: 100, state: 'active' })).toBe('normal')
  })
  it('正常 → normal', () => {
    expect(senderDisplayState({ done: 50, remote_done: 48, total: 100, state: 'active' })).toBe('normal')
  })
  it('remote_done undefined 非满格不算 backlog(旧任务容错)', () => {
    expect(senderDisplayState({ done: 30, total: 100, state: 'active' })).toBe('normal')
  })
})

describe('queueText', () => {
  it('queue_pos 非空 → 排队中 · 第 N 位', () => {
    expect(queueText(3, 'push')).toBe('排队中 · 第 3 位')
  })
  it('queue_pos null 回退 push 文案', () => {
    expect(queueText(null, 'push')).toBe('等待对方接收...')
  })
  it('queue_pos undefined 回退 pull 文案', () => {
    expect(queueText(undefined, 'pull')).toBe('排队中，连接对端...')
  })
})

describe('childRetryParams', () => {
  it('push-rel returns path+relDir from first valid item', () => {
    const req = { type: 'push-rel' as const, items: [['C:/dir/a.txt', 'sub'], ['C:/b.txt', '']] as [string, string][] }
    expect(childRetryParams(req)).toEqual({ path: 'C:/dir/a.txt', relDir: 'sub' })
  })

  it('push-rel with no valid item returns null', () => {
    expect(childRetryParams({ type: 'push-rel', items: [['C:/a.txt', '']] })).toBeNull()
  })

  it('push (absolute path) only needs path, relDir is empty string', () => {
    const req = { type: 'push' as const, paths: ['C:/abs/file.bin'] }
    expect(childRetryParams(req)).toEqual({ path: 'C:/abs/file.bin', relDir: '' })
  })

  it('push with empty paths returns null', () => {
    expect(childRetryParams({ type: 'push', paths: [] })).toBeNull()
  })

  it('pull uses req.path with empty relDir', () => {
    expect(childRetryParams({ type: 'pull', path: 'C:/d/f.mp4' })).toEqual({ path: 'C:/d/f.mp4', relDir: '' })
  })

  it('missing request returns null', () => {
    expect(childRetryParams(undefined)).toBeNull()
  })
})

describe('splitActiveHistory', () => {
  const mk = (over: Partial<import('../transferDisplay').TransferLike>): import('../transferDisplay').TransferLike => ({
    job_id: 'j',
    state: 'active',
    queue_pos: null,
    started_at_ms: 1000,
    finished_at_ms: null,
    ...over,
  })

  it('空输入返回两个空数组', () => {
    expect(splitActiveHistory([])).toEqual({ active: [], history: [] })
  })

  it('混合状态正确分组', () => {
    const jobs = [
      mk({ job_id: 'a', state: 'active' }),
      mk({ job_id: 'b', state: 'done' }),
      mk({ job_id: 'c', state: 'pending' }),
      mk({ job_id: 'd', state: 'paused' }),
      mk({ job_id: 'e', state: 'failed' }),
      mk({ job_id: 'f', state: 'interrupted' }),
      mk({ job_id: 'g', state: 'cancelling' }),
    ]
    const { active, history } = splitActiveHistory(jobs)
    expect(active.map((j) => j.job_id).sort()).toEqual(['a', 'c', 'd', 'g'])
    expect(history.map((j) => j.job_id).sort()).toEqual(['b', 'e', 'f'])
  })

  it('active 排序:active>cancelling>pending(queue_pos 升序,null 靠后)>paused', () => {
    const jobs = [
      mk({ job_id: 'paused1', state: 'paused' }),
      mk({ job_id: 'pend-null', state: 'pending', queue_pos: null }),
      mk({ job_id: 'canc', state: 'cancelling' }),
      mk({ job_id: 'pend-2', state: 'pending', queue_pos: 2 }),
      mk({ job_id: 'act', state: 'active' }),
      mk({ job_id: 'pend-1', state: 'pending', queue_pos: 1 }),
    ]
    const { active } = splitActiveHistory(jobs)
    expect(active.map((j) => j.job_id)).toEqual(['act', 'canc', 'pend-1', 'pend-2', 'pend-null', 'paused1'])
  })

  it('active 同级按 started_at_ms 升序(老的在前),null 靠后', () => {
    const jobs = [
      mk({ job_id: 'new', started_at_ms: 2000 }),
      mk({ job_id: 'null-ts', started_at_ms: null }),
      mk({ job_id: 'old', started_at_ms: 1000 }),
    ]
    const { active } = splitActiveHistory(jobs)
    expect(active.map((j) => j.job_id)).toEqual(['old', 'new', 'null-ts'])
  })

  it('history 按 finished_at_ms 降序(新的在前),null 靠后', () => {
    const jobs = [
      mk({ job_id: 'old', state: 'done', finished_at_ms: 1000 }),
      mk({ job_id: 'null-fin', state: 'failed', finished_at_ms: null }),
      mk({ job_id: 'new', state: 'done', finished_at_ms: 3000 }),
      mk({ job_id: 'mid', state: 'interrupted', finished_at_ms: 2000 }),
    ]
    const { history } = splitActiveHistory(jobs)
    expect(history.map((j) => j.job_id)).toEqual(['new', 'mid', 'old', 'null-fin'])
  })
})
