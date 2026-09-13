package com.localtrans.app.ui.transfers

/**
 * 速度平滑器(P1 打磨卡 R2,移植桌面 ui/src/lib/speedSmooth.ts,参数同构——双端 UI 口径一致):
 * - EMA 时间常数 τ=2s:alpha = 1 - exp(-dt/τ)
 * - 尖峰抑制:新值 > 3×当前 EMA 时按 1s 滑窗均速封顶并入(窗口未满或窗口整体
 *   未证实该速度都削;持续满窗的真实高速 avg≈raw 不受影响——防块边界瞬跳)
 * - 显示节流:平滑值最小 500ms 才刷新一次(进度条照常逐帧,速度文字不闪跳)
 *
 * push() 输入 FFI 泵的原始 speedBps + 任务 done 字节:进度导数优先作基准,
 * raw 仅作首拍回退。nowMs 注入以便单测确定性。
 */
class SpeedSmoother {
    /** EMA 时间常数(秒):约 2s 达 63% 响应 */
    val emaTauSecs: Double = 2.0

    /** 尖峰判定倍率 */
    val spikeFactor: Double = 3.0

    /** 尖峰滑窗时长(毫秒) */
    val spikeWindowMs: Long = 1000L

    /** 显示最小刷新间隔(毫秒) */
    val displayMinIntervalMs: Long = 500L

    /** 首样名义 dt(秒):按 4Hz 泵周期估,让启动爬升可见 */
    val nominalDtSecs: Double = 0.25

    /** 连续零速归零阈值(秒) */
    val zeroSnapSecs: Double = emaTauSecs

    /** 进度导数滑窗(毫秒):跨块周期取 done 差分,块间隙不产生假零速(与桌面 3s 窗同构) */
    val doneWindowMs: Long = 3000L

    private class Sample(val t: Long, val bps: Long)

    private var ema: Double = 0.0
    private var lastT: Long? = null
    private var window = ArrayDeque<Sample>()
    private var display: Double = 0.0
    private var displayAt: Long = Long.MIN_VALUE
    private var zeroSince: Long? = null

    /** 进度采样环(3s):done 差分出真实有效速率作输入基准 */
    private var doneRing = ArrayDeque<DoneSample>()

    private class DoneSample(val t: Long, val done: Long)

    /** 当前 EMA(未节流,诊断用) */
    val emaValue: Double get() = ema

    /** 当前显示值(≥500ms 才变) */
    val displayValue: Long get() = display.toLong()

    private fun alpha(dtSecs: Double): Double = 1.0 - Math.exp(-dtSecs / emaTauSecs)

    private fun step(prev: Double, new: Double, dtSecs: Double): Double =
        if (dtSecs <= 0.0) prev else prev + alpha(dtSecs) * (new - prev)

    private fun windowAvg(): Double {
        if (window.isEmpty()) return 0.0
        var sum = 0.0
        for (s in window) sum += s.bps
        return sum / window.size
    }

    /**
     * 送入一拍速度,返回平滑后的显示值(B/s)。
     *
     * 输入语义(与桌面 speedSmooth.ts 同步):优先用进度导数(3s 滑窗 done 差分,
     * 真实有效速率,跨块周期)——FFI 泵瞬时 speedBps 是突发档位,尾段停滞时会
     * 数倍虚高、慢速块节奏下逐拍差分又会低报;done 不可用(首拍)时用 rawBps。
     *
     * @param rawBps FFI 泵的瞬时速度 B/s(done 不可用时的回退输入)
     * @param nowMs 当前时刻(注入以便单测确定性)
     * @param doneBytes 本任务累计完成字节(可空)
     */
    fun push(rawBps: Long, nowMs: Long, doneBytes: Long? = null): Long {
        val dtSecs = lastT?.let { (nowMs - it) / 1000.0 }

        // 输入基准:进度导数优先(3s 滑窗差分,跨块周期),负差分(重试回退)回退 raw
        var base = rawBps.toDouble()
        if (doneBytes != null) {
            while (doneRing.isNotEmpty() && nowMs - doneRing.first().t > doneWindowMs) doneRing.removeFirst()
            doneRing.addLast(DoneSample(nowMs, doneBytes))
            if (doneRing.size >= 2) {
                val first = doneRing.first()
                val last = doneRing.last()
                val spanSecs = (last.t - first.t) / 1000.0
                val deriv = (last.done - first.done) / spanSecs
                if (spanSecs > 0 && deriv >= 0) base = deriv
            }
        }

        // 滑窗只留 spikeWindowMs 内的样本(存基准值,供尖峰封顶)
        while (window.isNotEmpty() && nowMs - window.first().t > spikeWindowMs) window.removeFirst()
        lastT = nowMs
        window.addLast(Sample(nowMs, base.toLong()))

        // 尖峰抑制:疑似瞬跳(>3×EMA)→ 按 1s 滑窗均速封顶并入。
        // 窗口未满或窗口整体未证实该速度(均速<原值)都削;持续满窗的真实高速
        // avg≈base 不受影响。ema==0(首样/暂停归零后)同样生效:窗口均速就是基线。
        var effective = base
        if (base > spikeFactor * ema) {
            effective = minOf(base, windowAvg())
        }

        // 零速累计:满 τ 直接归零(终态/暂停及时清零,不留指数长尾)
        if (base == 0.0) {
            if (zeroSince == null) zeroSince = nowMs
        } else {
            zeroSince = null
        }

        ema = when {
            dtSecs == null -> step(0.0, effective, nominalDtSecs) // 首样:名义 4Hz 周期从 0 爬升
            zeroSince != null && nowMs - zeroSince!! >= (zeroSnapSecs * 1000).toLong() -> 0.0
            else -> step(ema, effective, dtSecs)
        }

        // 显示节流:≥500ms 才刷新;归零是重要状态变化,豁免节流立即上屏
        val snapToZero = ema == 0.0 && display != 0.0
        if (displayAt == Long.MIN_VALUE || snapToZero || nowMs - displayAt >= displayMinIntervalMs) {
            display = ema
            displayAt = nowMs
        }
        return display.toLong()
    }

    /** 任务终态/删除/重开时清状态(下次首样重新爬升) */
    fun reset() {
        ema = 0.0
        lastT = null
        window.clear()
        display = 0.0
        displayAt = Long.MIN_VALUE
        zeroSince = null
        doneRing.clear()
    }
}
