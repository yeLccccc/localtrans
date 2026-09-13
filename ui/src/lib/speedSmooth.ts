/**
 * 速度平滑纯函数模块(P1 打磨卡 R2)
 *
 * 数据链:Rust 4Hz 泵(speed_bps 逐事件,不动)→ 本模块 EMA+滑窗+尖峰抑制+显示节流
 * → store 的 speed_bps(平滑后显示值)→ TransferItem 渲染(ETA 随动)。
 *
 * 参数与 Android 端 SpeedSmoother.kt 同构(双端 UI 口径一致):
 * - 输入基准:进度导数(3s 滑窗 done 差分,真实有效速率)优先,raw speed_bps 仅作首拍回退
 * - EMA 时间常数 τ=2s:alpha = 1 - exp(-dt/τ)
 * - 尖峰抑制:新值 > 3×当前 EMA 时按 1s 滑窗均速封顶并入(窗口未满或窗口整体
 *   未证实该速度都削;持续满窗的真实高速 avg≈raw 不受影响——防 4MB 块边界瞬跳)
 * - 显示节流:平滑值最小 500ms 才刷新一次到 display(进度条宽度不受影响,
 *   CSS 补间 0.28s 照旧)
 */

/** EMA 时间常数(秒):约 2s 达 63% 响应 */
export const EMA_TAU_SECS = 2

/** 尖峰判定倍率:新值 > 3×EMA 视为疑似块边界瞬跳 */
export const SPIKE_FACTOR = 3

/** 尖峰滑窗时长(毫秒):窗口 <1s 时尖峰未被证实 → 按窗口均速并入 */
export const SPIKE_WINDOW_MS = 1000

/** 显示最小刷新间隔(毫秒):平滑值 ≥500ms 才更新 display */
export const DISPLAY_MIN_INTERVAL_MS = 500

/** 首样名义 dt(秒):无前序时间戳时按 4Hz 泵周期估,让启动爬升可见 */
export const NOMINAL_DT_SECS = 0.25

/** 连续零速归零阈值(秒):base 持续 0 达 τ 后 EMA 直接清零(结束归零不等长尾) */
export const ZERO_SNAP_SECS = EMA_TAU_SECS

/** 进度导数滑窗(毫秒):跨块周期取 done 差分,块间隙不产生假零速(与 Android 3s 窗同构) */
export const DONE_WINDOW_MS = 3000

/** EMA 步进系数:alpha = 1 - exp(-dt/τ) */
export function emaAlpha(dtSecs: number): number {
  return 1 - Math.exp(-dtSecs / EMA_TAU_SECS)
}

/**
 * 纯 EMA 步进。dtSecs<=0(同拍重复事件/时钟回拨)不更新返回原值。
 * 新值长 dt 大时 alpha→1 自然快速跟随(暂停恢复后不拖泥带水)。
 */
export function smoothSpeed(prevEma: number, newValue: number, dtSecs: number): number {
  if (!(dtSecs > 0)) return prevEma
  const a = emaAlpha(dtSecs)
  return prevEma + a * (newValue - prevEma)
}

/** 滑窗样本(4Hz 泵的逐拍瞬时速度) */
export interface SpeedSample {
  /** 采样时刻(ms,单调比较用) */
  t: number
  /** 瞬时速度 B/s */
  bps: number
}

/** 窗口均速:样本算术平均(4Hz 等间隔下≈字节/时间) */
export function windowAvgSpeed(samples: SpeedSample[]): number {
  if (samples.length === 0) return 0
  let sum = 0
  for (const s of samples) sum += s.bps
  return sum / samples.length
}

/**
 * 速度平滑器(每任务一个):EMA + 1s 滑窗尖峰抑制 + 500ms 显示节流。
 * push() 输入 Rust 原始 speed_bps,返回可直接上屏的平滑显示值。
 */
export class SpeedSmoother {
  private ema = 0
  private lastT: number | null = null
  private window: SpeedSample[] = []
  private display = 0
  private displayAt = -Infinity
  /** 连续零速起点(null=非零速):累计满 τ 直接归零 */
  private zeroSince: number | null = null
  /** 进度采样环(3s):done 差分出真实有效速率作输入基准 */
  private doneRing: { t: number; done: number }[] = []

  /** 当前 EMA(未节流,测试/诊断用) */
  get emaValue(): number {
    return this.ema
  }

  /** 当前显示值(≥500ms 才变) */
  get displayValue(): number {
    return this.display
  }

/**
 * 送入一拍速度,返回平滑后的显示值。
 *
 * 输入语义(R2 实测修正):优先用进度导数(3s 滑窗 done 差分,真实有效速率,
 * 跨块周期不产生假零速)——发送泵的瞬时 speed_bps 是突发档位(64-131MB/s 帧间零),
 * 尾段停滞时会数倍虚高、慢速块节奏下逐拍差分又会低报;done 不可用(首拍)时用 rawBps。
 *
 * @param rawBps Rust 4Hz 泵的瞬时速度 B/s(done 不可用时的回退输入)
 * @param nowMs 当前时刻(注入以便单测确定性)
 * @param doneBytes 本任务累计完成字节(可空;3s 滑窗差分出进度导数)
 */
  push(rawBps: number, nowMs: number, doneBytes?: number): number {
    const dtSecs = this.lastT === null ? null : (nowMs - this.lastT) / 1000

    // 输入基准:进度导数优先(3s 滑窗差分,跨块周期),负差分(重试回退)回退 raw
    let base = rawBps
    if (doneBytes !== undefined) {
      this.doneRing = this.doneRing.filter((s) => nowMs - s.t <= DONE_WINDOW_MS)
      this.doneRing.push({ t: nowMs, done: doneBytes })
      if (this.doneRing.length >= 2) {
        const first = this.doneRing[0]
        const last = this.doneRing[this.doneRing.length - 1]
        const spanSecs = (last.t - first.t) / 1000
        const deriv = (last.done - first.done) / spanSecs
        if (spanSecs > 0 && deriv >= 0) base = deriv
      }
    }

    // 滑窗只留 SPIKE_WINDOW_MS 内的样本(存基准值,供尖峰封顶)
    this.window = this.window.filter((s) => nowMs - s.t <= SPIKE_WINDOW_MS)
    this.lastT = nowMs
    this.window.push({ t: nowMs, bps: base })

    // 尖峰抑制:疑似瞬跳(>3×EMA)→ 按 1s 滑窗均速封顶并入。
    // 窗口未满或窗口整体未证实该速度(均速<原值)都削;
    // 持续满窗的真实高速 avg≈base 不受影响——趋势跟随不受伤。
    let effective = base
    if (base > SPIKE_FACTOR * this.ema) {
      effective = Math.min(base, windowAvgSpeed(this.window))
    }

    // 零速累计:满 τ 直接归零(终态/暂停及时清零,不留指数长尾)
    if (base === 0) {
      if (this.zeroSince === null) this.zeroSince = nowMs
    } else {
      this.zeroSince = null
    }

    if (dtSecs === null) {
      // 首样:按名义 4Hz 周期从 0 起步(启动爬升可见,不一步跳满)
      this.ema = smoothSpeed(0, effective, NOMINAL_DT_SECS)
    } else if (this.zeroSince !== null && nowMs - this.zeroSince >= ZERO_SNAP_SECS * 1000) {
      this.ema = 0
    } else {
      this.ema = smoothSpeed(this.ema, effective, dtSecs)
    }

    // 显示节流:≥500ms 才刷新;归零是重要状态变化,豁免节流立即上屏
    const snapToZero = this.ema === 0 && this.display !== 0
    if (this.displayAt === -Infinity || snapToZero || nowMs - this.displayAt >= DISPLAY_MIN_INTERVAL_MS) {
      this.display = this.ema
      this.displayAt = nowMs
    }
    return this.display
  }

  /** 任务终态/删除/重开时清状态(下次首样重新爬升) */
  reset(): void {
    this.ema = 0
    this.lastT = null
    this.window = []
    this.display = 0
    this.displayAt = -Infinity
    this.zeroSince = null
    this.doneRing = []
  }
}

/**
 * 速度格式化(口径与原 TransferItem 局部实现一致):
 * 1024 进制,一位小数,B/KB/MB/GB/s
 */
export function formatSpeed(bps: number): string {
  if (bps === 0) return '0 B/s'

  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let value = bps
  let unitIndex = 0

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex++
  }

  return `${value.toFixed(1)} ${units[unitIndex]}`
}
