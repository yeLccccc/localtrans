package com.localtrans.app.ui.transfers

import org.junit.Assert.*
import org.junit.Test

/**
 * 速度平滑器单测(P1 R2,与桌面 speedSmooth.test.ts 同口径):
 * EMA 序列/尖峰抑制边界/窗口/归零/显示节流
 */
class SpeedSmootherTest {

    companion object {
        const val MB = 1024L * 1024
        const val HZ_DT = 250L // 4Hz 泵周期(ms)
    }

    // ===== EMA 序列 =====

    @Test
    fun `constant input converges to value`() {
        val sm = SpeedSmoother()
        var t = 0L
        repeat(200) {
            t += HZ_DT
            sm.push(4 * MB, t)
        }
        assertEquals(4.0 * MB, sm.emaValue, 4.0 * MB * 0.01)
    }

    @Test
    fun `first sample ramps from zero not full jump`() {
        val sm = SpeedSmoother()
        val out = sm.push(10 * MB, 0)
        assertTrue("首样应大于 0", out > 0)
        assertTrue("首样不一步跳满(名义 dt 从 0 爬升)", out < 2 * MB)
    }

    @Test
    fun `cold burst after zero baseline is suppressed by window average`() {
        val sm = SpeedSmoother()
        var t = 0L
        // 卡片 pending 阶段零速 ≥2s → ema 已归零(无基线)
        repeat(12) { t += HZ_DT; sm.push(0, t) }
        assertEquals(0.0, sm.emaValue, 0.0)
        // 接收确认后 Rust 泵首发瞬跳 128MB/s
        t += HZ_DT
        val out = sm.push(128 * MB, t)
        assertTrue("零基线首发瞬跳应被窗口均速削平: $out", out in 1 until 6 * MB)
    }

    @Test
    fun `long gap catches up fast after resume`() {
        val sm = SpeedSmoother()
        var t = 0L
        repeat(8) { t += HZ_DT; sm.push(MB, t) }
        t += 20_000 // 暂停 20s
        val out = sm.push(10 * MB, t)
        assertTrue("长 dt 后 alpha≈1 应快速跟上: $out", out > 9 * MB * 0.9)
    }

    // ===== 尖峰抑制 =====

    @Test
    fun `single spike above 3x ema within window is averaged down`() {
        val sm = SpeedSmoother()
        var t = 0L
        repeat(12) { t += HZ_DT; sm.push(MB, t) } // 3s 稳态 1MB/s
        val steady = sm.emaValue
        assertTrue(steady > 0.7 * MB)
        t += HZ_DT
        val out = sm.push(10 * MB, t) // 块边界瞬跳
        // 窗口均速 (1+1+1+10)/4=3.25MB/s 量级——远低于 10MB/s,但高于稳态(在爬升)
        assertTrue("尖峰应被削: $out", out < 4 * MB)
        assertTrue(out > steady)
    }

    @Test
    fun `sustained high speed beyond window is followed as real bandwidth`() {
        val sm = SpeedSmoother()
        var t = 0L
        repeat(4) { t += HZ_DT; sm.push(MB, t) }
        val steadyEma = sm.emaValue
        repeat(6) { t += HZ_DT; sm.push(10 * MB, t) } // 持续 1.5s 高速
        assertTrue(
            "持续高速 ≥1s 应被跟随: ${sm.emaValue}",
            sm.emaValue > spikeFactorOf(steadyEma) && sm.emaValue > 3 * MB,
        )
    }

    private fun spikeFactorOf(v: Double) = v * 3.0

    // ===== 归零 =====

    @Test
    fun `zeros sustained for tau snap ema to zero`() {
        val sm = SpeedSmoother()
        var t = 0L
        repeat(8) { t += HZ_DT; sm.push(4 * MB, t) }
        assertTrue(sm.emaValue > MB)
        // 零速累计 2s(zeroSince=t2000,末拍 t=4000 恰满 τ)
        t += HZ_DT
        repeat(9) { t += HZ_DT; sm.push(0, t) }
        assertEquals(0L, sm.displayValue.toLong())
        assertEquals(0.0, sm.emaValue, 0.0)
    }

    @Test
    fun `zero snap bypasses display throttle`() {
        val sm = SpeedSmoother()
        var t = 0L
        repeat(8) { t += HZ_DT; sm.push(4 * MB, t) }
        assertTrue(sm.displayValue > 0)
        t += 200
        sm.push(0, t)
        t += 2000
        assertEquals(0L, sm.push(0, t))
    }

    @Test
    fun `reset clears state and ramps again`() {
        val sm = SpeedSmoother()
        sm.push(4 * MB, 0)
        sm.reset()
        assertEquals(0L, sm.displayValue)
        val out = sm.push(4 * MB, 1000)
        assertTrue("reset 后重新爬升: $out", out < MB)
    }

    // ===== 显示节流(500ms) =====

    @Test
    fun `display value updates at most every 500ms`() {
        val sm = SpeedSmoother()
        var t = 0L
        val first = sm.push(8 * MB, t)
        repeat(4) {
            t += 100
            assertEquals("500ms 内 display 不变", first, sm.push(8 * MB, t))
        }
        t += 200
        val out = sm.push(8 * MB, t)
        assertTrue(out >= first)
    }

    @Test
    fun `no single display point above 3x median over 20s with block spikes`() {
        val sm = SpeedSmoother()
        val series = ArrayList<Long>()
        var t = 0L
        // 线速 20MB/s,每 2s 一次 4× 块边界瞬跳
        repeat(80) {
            t += HZ_DT
            series.add(sm.push(if (it % 8 == 7) 80 * MB else 20 * MB, t))
        }
        val sorted = series.sorted()
        val median = sorted[sorted.size / 2]
        for (v in series) {
            assertTrue("单点 $v 超 3×中位数 $median", v <= 3 * median)
        }
        // 趋势跟随:末段明显高于首段
        assertTrue(series.last() > series.first() * 3)
    }

    // ===== 进度导数基准(与桌面 speedSmooth.test.ts 同步,2026-09-08 run-all 尾段虚高回归)=====

    @Test
    fun `done derivative bounds display when pump reports burst tier`() {
        val sm = SpeedSmoother()
        var t = 0L
        var done = 0L
        // 每 2s 真实推进 8MB(=4MB/s),泵每拍都报 64MB/s 突发档
        repeat(40) {
            t += HZ_DT
            if (it % 8 == 0) done += 8 * MB
            sm.push(64 * MB, t, done)
        }
        assertTrue("显示应落在真实速率量级: ${sm.emaValue}", sm.emaValue < 6 * MB)
        assertTrue(sm.emaValue > 0.25 * MB)
    }

    @Test
    fun `done stall decays display even when pump reports high tier`() {
        val sm = SpeedSmoother()
        var t = 0L
        var done = 0L
        repeat(12) { t += HZ_DT; done += MB / 4; sm.push(MB, t, done) }
        assertTrue(sm.emaValue > 0.5 * MB)
        // 停滞 5s:done 不再增长,泵仍报 8MB/s
        repeat(20) { t += HZ_DT; sm.push(8 * MB, t, done) }
        assertEquals(0.0, sm.emaValue, 0.0)
    }

    @Test
    fun `negative done delta falls back to raw input`() {
        val sm = SpeedSmoother()
        var t = 0L
        var done = 10L * MB
        repeat(8) { t += HZ_DT; done += MB; sm.push(4 * MB, t, done) }
        val before = sm.emaValue
        t += HZ_DT
        sm.push(4 * MB, t, 2 * MB) // done 回退(重试从头):负差分 → 回退 raw
        assertTrue("raw 4MB/s 并入后 EMA 继续爬: ${sm.emaValue}", sm.emaValue > before)
    }

    // ===== 与桌面参数同构 =====

    @Test
    fun `parameters match desktop speedSmooth constants`() {
        val sm = SpeedSmoother()
        assertEquals(2.0, sm.emaTauSecs, 0.0)
        assertEquals(3.0, sm.spikeFactor, 0.0)
        assertEquals(1000L, sm.spikeWindowMs)
        assertEquals(500L, sm.displayMinIntervalMs)
        assertEquals(0.25, sm.nominalDtSecs, 0.0)
    }
}
