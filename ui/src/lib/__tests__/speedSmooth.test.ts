import { describe, it, expect } from 'vitest'
import {
  EMA_TAU_SECS,
  SPIKE_FACTOR,
  DISPLAY_MIN_INTERVAL_MS,
  emaAlpha,
  smoothSpeed,
  windowAvgSpeed,
  SpeedSmoother,
  formatSpeed,
  type SpeedSample,
} from '../speedSmooth'

const MB = 1024 * 1024

describe('emaAlpha / smoothSpeed', () => {
  it('dt=τ 时 alpha = 1-1/e ≈ 0.632', () => {
    expect(emaAlpha(EMA_TAU_SECS)).toBeCloseTo(1 - Math.exp(-1), 12)
  })

  it('dt<=0 不更新(同拍重复事件/时钟回拨)', () => {
    expect(smoothSpeed(100, 999, 0)).toBe(100)
    expect(smoothSpeed(100, 999, -0.5)).toBe(100)
  })

  it('恒定输入序列收敛到该值(EMA 序列)', () => {
    let ema = 0
    for (let i = 0; i < 200; i++) {
      ema = smoothSpeed(ema, 4 * MB, 0.25)
    }
    expect(ema).toBeCloseTo(4 * MB, 0)
  })

  it('τ=2s:一拍 250ms 只走 ~11.8%(启动爬升平缓)', () => {
    const ema = smoothSpeed(0, 10 * MB, 0.25)
    expect(ema).toBeGreaterThan(1 * MB)
    expect(ema).toBeLessThan(1.5 * MB)
  })

  it('长 dt 恢复:alpha→1 快速跟上(暂停恢复不拖尾)', () => {
    const ema = smoothSpeed(1 * MB, 10 * MB, 20)
    expect(ema).toBeGreaterThan(9.9 * MB)
  })
})

describe('windowAvgSpeed', () => {
  it('空窗口 → 0', () => {
    expect(windowAvgSpeed([])).toBe(0)
  })
  it('算术平均', () => {
    const samples: SpeedSample[] = [
      { t: 0, bps: 1 * MB },
      { t: 250, bps: 2 * MB },
      { t: 500, bps: 3 * MB },
    ]
    expect(windowAvgSpeed(samples)).toBeCloseTo(2 * MB, 0)
  })
})

describe('SpeedSmoother 尖峰抑制', () => {
  it('稳态 1MB/s 后突跳 10MB/s(窗口 <1s)→ 并入窗口均速而非直用', () => {
    const sm = new SpeedSmoother()
    // 预热:12 拍稳态 1MB/s(4Hz,3s)让 EMA 收敛近稳态
    for (let i = 0; i < 12; i++) sm.push(MB, i * 250)
    const steady = sm.emaValue
    expect(steady).toBeGreaterThan(0.7 * MB)

    // 块边界瞬跳 10MB/s
    const out = sm.push(10 * MB, 3000)
    // 窗口均速 = (1+1+1+10)/4 = 3.25MB/s 量级,远低于 10MB/s
    expect(out).toBeLessThan(4 * MB)
    expect(out).toBeGreaterThan(steady) // 但确实在爬升
  })

  it('持续高速 ≥1s → 视为真实带宽,EMA 跟上(>3×原稳态)', () => {
    const sm = new SpeedSmoother()
    for (let i = 0; i < 4; i++) sm.push(MB, i * 250)
    // 持续 10MB/s 达 1.5s(窗口满 1s 后不再削峰)
    for (let i = 0; i < 6; i++) sm.push(10 * MB, 1000 + i * 250)
    expect(sm.emaValue).toBeGreaterThan(3 * MB)
    expect(sm.emaValue).toBeGreaterThan(SPIKE_FACTOR * MB)
  })

  it('首样按名义 dt 从 0 爬升(不一步跳满)', () => {
    const sm = new SpeedSmoother()
    const out = sm.push(10 * MB, 0)
    expect(out).toBeGreaterThan(0)
    expect(out).toBeLessThan(2 * MB)
  })

  it('零速归零后首发泵尖峰(挂起→开传)按窗口均速并入,不直用', () => {
    const sm = new SpeedSmoother()
    let t = 0
    // 卡片 pending 阶段零速 ≥2s → ema 已归零(无基线)
    for (let i = 0; i < 12; i++) { t += 250; sm.push(0, t) }
    expect(sm.emaValue).toBe(0)
    // 接收确认后 Rust 泵首发瞬跳 128MB/s(首发窗口 dt 极小)
    t += 250
    const out = sm.push(128 * MB, t)
    // 窗口 {0,0,0,128MB} 均速 32MB/s × 名义首样系数 0.117 ≈ 3.8MB/s,远低于直用值
    expect(out).toBeGreaterThan(0)
    expect(out).toBeLessThan(6 * MB)
  })
})

describe('SpeedSmoother 进度导数基准(2026-09-08 run-all 尾段虚高回归)', () => {
  it('泵突发但 done 匀速推进:显示跟随真实速率,不随泵档位虚高', () => {
    const sm = new SpeedSmoother()
    let t = 0
    let done = 0
    for (let i = 0; i < 40; i++) {
      t += 250
      if (i % 8 === 0) done += 8 * MB // 每 2s 真实推进 8MB = 4MB/s
      sm.push(64 * MB, t, done) // 泵报 64MB/s 突发档
    }
    expect(sm.emaValue).toBeLessThan(6 * MB) // 真实速率量级,远低于 64MB/s 泵档
    expect(sm.emaValue).toBeGreaterThan(0.25 * MB)
  })

  it('尾段 done 停滞(泵仍报高档):显示衰减不虚高', () => {
    const sm = new SpeedSmoother()
    let t = 0
    let done = 0
    for (let i = 0; i < 12; i++) { t += 250; done += 0.25 * MB; sm.push(MB, t, done) }
    expect(sm.emaValue).toBeGreaterThan(0.5 * MB)
    // 停滞 20 拍(5s):done 不再增长,泵仍报 8MB/s
    for (let i = 0; i < 20; i++) { t += 250; sm.push(8 * MB, t, done) }
    expect(sm.emaValue).toBe(0) // 零速满 τ 归零
  })

  it('done 负差分(重试回退)回退 raw 输入', () => {
    const sm = new SpeedSmoother()
    let t = 0
    let done = 10 * MB
    for (let i = 0; i < 8; i++) { t += 250; done += MB; sm.push(4 * MB, t, done) }
    const before = sm.emaValue
    // done 回退(重试从头):负差分 → 本拍回退用 raw
    t += 250
    sm.push(4 * MB, t, 2 * MB)
    expect(sm.emaValue).toBeGreaterThan(before) // raw 4MB/s 并入,EMA 继续爬
  })
})

describe('SpeedSmoother 归零', () => {
  it('raw 持续 0 达 τ → EMA 直接清零(结束归零)', () => {
    const sm = new SpeedSmoother()
    for (let i = 0; i < 8; i++) sm.push(4 * MB, i * 250)
    expect(sm.emaValue).toBeGreaterThan(1 * MB)
    // 0 持续 2s+:zeroSince=t2000,第 9 拍 t=4000 恰满 τ 触发归零
    let t = 2000
    for (let i = 0; i < 9; i++) {
      t += 250
      sm.push(0, t)
    }
    expect(sm.emaValue).toBe(0)
    expect(sm.displayValue).toBe(0)
  })

  it('归零豁免显示节流立即上屏', () => {
    const sm = new SpeedSmoother()
    let t = 0
    for (let i = 0; i < 8; i++) { t += 250; sm.push(4 * MB, t) }
    const before = sm.displayValue
    expect(before).toBeGreaterThan(0)
    // 长零:清零发生在节流窗口内也必须立刻反映
    t += 200
    sm.push(0, t)
    t += 2000
    const out = sm.push(0, t)
    expect(out).toBe(0)
  })

  it('reset 后状态清空', () => {
    const sm = new SpeedSmoother()
    sm.push(4 * MB, 0)
    sm.reset()
    expect(sm.emaValue).toBe(0)
    expect(sm.displayValue).toBe(0)
    expect(sm.push(4 * MB, 1000)).toBeLessThan(1 * MB) // 重新爬升
  })
})

describe('SpeedSmoother 显示节流(500ms)', () => {
  it('display 至少间隔 500ms 才刷新', () => {
    const sm = new SpeedSmoother()
    let t = 0
    sm.push(8 * MB, t)
    const first = sm.displayValue
    // 100ms 一拍持续变化,display 500ms 内不变
    for (let i = 0; i < 4; i++) {
      t += 100
      const out = sm.push(8 * MB, t)
      expect(out).toBe(first)
    }
    // 跨过 500ms 后允许刷新
    t += 200
    const out = sm.push(8 * MB, t)
    expect(out).toBeGreaterThanOrEqual(first)
    expect(t - 0).toBeGreaterThanOrEqual(DISPLAY_MIN_INTERVAL_MS)
  })

  it('4Hz 泵送 20s:display 单调爬升无 >3× 中位数单点尖刺', () => {
    const sm = new SpeedSmoother()
    const series: number[] = []
    // 模拟线速 20MB/s,块边界每 2s 一次 4× 瞬跳
    for (let i = 0; i < 80; i++) {
      const raw = i % 8 === 7 ? 80 * MB : 20 * MB
      series.push(sm.push(raw, i * 250))
    }
    const sorted = [...series].sort((a, b) => a - b)
    const median = sorted[Math.floor(sorted.length / 2)]
    for (const v of series) {
      expect(v).toBeLessThanOrEqual(3 * median)
    }
    // 趋势跟随:末段明显高于首段(爬升到位)
    expect(series[series.length - 1]).toBeGreaterThan(series[0] * 3)
  })
})

describe('formatSpeed', () => {
  it('口径与原 TransferItem 实现一致(1024 进制,一位小数)', () => {
    expect(formatSpeed(0)).toBe('0 B/s')
    expect(formatSpeed(512)).toBe('512.0 B/s')
    expect(formatSpeed(1024)).toBe('1.0 KB/s')
    expect(formatSpeed(1.5 * MB)).toBe('1.5 MB/s')
    expect(formatSpeed(2.25 * 1024 * MB)).toBe('2.3 GB/s') // 2.25 一位小数四舍五入
  })
})
