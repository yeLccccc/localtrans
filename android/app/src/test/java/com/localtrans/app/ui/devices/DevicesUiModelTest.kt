package com.localtrans.app.ui.devices

import org.junit.Assert.*
import org.junit.Test
import uniffi.localtrans_ffi.ChannelDto

class DevicesUiModelTest {
    @Test
    fun `private lan ips detected`() {
        assertTrue(isPrivateLanIp("192.168.1.5"))
        assertTrue(isPrivateLanIp("10.1.2.3"))
        assertTrue(isPrivateLanIp("172.16.0.1"))
        assertTrue(isPrivateLanIp("172.31.255.255"))
        assertTrue(isPrivateLanIp("127.0.0.1"))
    }

    @Test
    fun `public and malformed ips rejected`() {
        assertFalse(isPrivateLanIp("8.8.8.8"))
        assertFalse(isPrivateLanIp("172.32.0.1"))
        assertFalse(isPrivateLanIp("100.64.0.1")) // CGNAT,移动网络典型段
        assertFalse(isPrivateLanIp("abc"))
        assertFalse(isPrivateLanIp(""))
        assertFalse(isPrivateLanIp("::1"))
        assertFalse(isPrivateLanIp("256.256.256.256"))
        assertFalse(isPrivateLanIp("1.2.3.4.5"))
    }

    // ===== M3c T1 通道标签三态(与 PC DeviceCard 同构) =====

    private fun channel(
        fingerprint: String = "FP1",
        addr: String = "192.168.1.23:47601",
        viaRelay: Boolean = false,
        rttMs: ULong? = 2uL,
        current: Boolean = true
    ) = ChannelDto(
        fingerprint = fingerprint,
        addr = addr,
        viaRelay = viaRelay,
        rttMs = rttMs,
        estBps = 90_000_000uL,
        lossRate = 0.0,
        current = current,
        scoreReady = true,
        probeDisabled = false,
        ageSecs = 3uL
    )

    @Test
    fun `direct channel label with rtt`() {
        val c = channelFor(listOf(channel()), "FP1", "192.168.1.23:47601")
        assertEquals(ChannelKind.DIRECT, c?.kind)
        assertEquals("直连 · 2ms", channelLabelText(c))
    }

    @Test
    fun `relay channel label with rtt`() {
        val c = channelFor(listOf(channel(viaRelay = true, addr = "10.8.0.17:51022", rttMs = 120uL)), "FP1", "192.168.1.23:47601")
        assertEquals(ChannelKind.RELAY, c?.kind)
        assertEquals("经中继 · 120ms", channelLabelText(c))
    }

    @Test
    fun `no record means unknown`() {
        assertNull(channelFor(emptyList(), "FP1", "192.168.1.23:47601"))
        assertEquals("未知", channelLabelText(null))
    }

    @Test
    fun `record without rtt shows path only`() {
        val c = channelFor(listOf(channel(rttMs = null)), "FP1", "192.168.1.23:47601")
        assertEquals("直连", channelLabelText(c))
    }

    @Test
    fun `current record wins over addr match`() {
        val records = listOf(
            channel(addr = "10.8.0.17:51022", viaRelay = true, rttMs = 120uL, current = false),
            channel(addr = "192.168.1.23:47601", rttMs = 2uL, current = false)
        )
        // current 缺失 → 按发现地址匹配直连记录
        val c = channelFor(records, "FP1", "192.168.1.23:47601")
        assertEquals(ChannelKind.DIRECT, c?.kind)
        // current 在场 → 优先当前通道(中继)
        val c2 = channelFor(
            listOf(records[0].copy(current = true)),
            "FP1", "192.168.1.23:47601"
        )
        assertEquals(ChannelKind.RELAY, c2?.kind)
    }

    // ===== M3c T2 通道面板(与 PC ChannelPanel 同构) =====

    @Test
    fun `panel rows current first then by addr and filtered by fingerprint`() {
        val records = listOf(
            channel(addr = "10.8.0.17:51022", viaRelay = true, rttMs = 120uL, current = false),
            channel(fingerprint = "FP2", addr = "9.9.9.9:1", current = false),
            channel(addr = "192.168.1.23:47601", rttMs = 2uL, current = true),
        )
        val rows = channelRows(records, "FP1")
        assertEquals("只含本设备记录", 2, rows.size)
        assertEquals("当前通道置首", "192.168.1.23:47601", rows[0].addr)
        assertEquals("10.8.0.17:51022", rows[1].addr)
        assertTrue(rows[0].current)
        assertTrue(rows[1].viaRelay)
    }

    @Test
    fun `panel est bps humanize`() {
        assertEquals("90.0 Mbps", formatEstBps(90_000_000uL))
        assertEquals("8.0 Mbps", formatEstBps(8_000_000uL))
        assertEquals("850 Kbps", formatEstBps(850_000uL))
        assertEquals("999 bps", formatEstBps(999uL))
        assertEquals("—", formatEstBps(null))
        assertEquals("—", formatEstBps(0uL))
    }

    @Test
    fun `panel age formatting`() {
        assertEquals("刚刚", formatAge(0uL))
        assertEquals("刚刚", formatAge(4uL))
        assertEquals("42s前", formatAge(42uL))
        assertEquals("2分前", formatAge(120uL))
        assertEquals("3分前", formatAge(195uL))
    }
}
