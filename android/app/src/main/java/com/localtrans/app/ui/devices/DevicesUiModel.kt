package com.localtrans.app.ui.devices

import uniffi.localtrans_ffi.ChannelDto

/**
 * UI state for pairing flow
 */
data class PairingUiState(
    val fingerprint: String,
    val peerName: String,
    val code: String = "",
    val state: PairingState,
    val reason: String = "",
    /** CODE_ENTRY 截止时刻(epoch ms;0=无期限) */
    val deadlineEpochMs: Long = 0L
)

/**
 * Pairing flow states
 */
enum class PairingState {
    CONSENT_REQUESTED,
    CODE_SHOWN,
    CODE_ENTRY,
    WAITING_CONSENT,
    SUCCESS,
    FAILED
}

/**
 * Manual probe session state (null = no session)
 */
data class ManualProbeUiState(
    val state: ProbeState,
    val target: String,
    val message: String = ""
)

enum class ProbeState {
    PROBING,
    FOUND,
    NOT_FOUND,
    ERROR
}

/**
 * M3c T1 通道标签:路径种类 + 最近 RTT(未测得为 null)
 */
enum class ChannelKind { DIRECT, RELAY }

data class ChannelUi(
    val kind: ChannelKind,
    val rttMs: ULong?
)

/**
 * M3c T1:从通道表挑该设备的记录(与 PC 端 DeviceCard 同构):
 * 优先当前通道记录(current=最近一次成功连接地址),其次按发现地址匹配。
 * 无记录返回 null(UI 显示「未知」——离线常驻卡/未建会话)。
 */
fun channelFor(records: List<ChannelDto>, fingerprint: String, addr: String): ChannelUi? {
    val rec = records.filter { it.fingerprint == fingerprint }
        .find { it.current }
        ?: records.firstOrNull { it.addr == addr }
        ?: return null
    return ChannelUi(
        kind = if (rec.viaRelay) ChannelKind.RELAY else ChannelKind.DIRECT,
        rttMs = rec.rttMs
    )
}

/**
 * M3c T1:通道标签文案(与 PC 端同构):「直连 · 2ms」/「经中继 · 120ms」/
 * 无记录=「未知」;记录在但 RTT 未测得(探测进行中)只显示路径。
 */
fun channelLabelText(channel: ChannelUi?): String = when {
    channel == null -> "未知"
    channel.rttMs != null ->
        if (channel.kind == ChannelKind.DIRECT) "直连 · ${channel.rttMs}ms"
        else "经中继 · ${channel.rttMs}ms"
    channel.kind == ChannelKind.DIRECT -> "直连"
    else -> "经中继"
}

/**
 * M3c T2 通道面板:一行地址明细(与 PC ChannelPanel 行同构)。
 * current=当前使用通道(✓ 标记);path/rtt/est/loss/age 为展示字段。
 */
data class ChannelRowUi(
    val addr: String,
    val viaRelay: Boolean,
    val rttMs: ULong?,
    val estBps: ULong?,
    val lossRate: Double,
    val current: Boolean,
    val ageSecs: ULong
)

/**
 * M3c T2:从通道表取该设备的全部记录(面板每地址一行)。
 * 当前通道置首,其余按地址稳定排序(与 PC ChannelPanel 同口径)。
 */
fun channelRows(records: List<ChannelDto>, fingerprint: String): List<ChannelRowUi> =
    records.filter { it.fingerprint == fingerprint }
        .sortedWith(compareBy({ !it.current }, { it.addr }))
        .map {
            ChannelRowUi(
                addr = it.addr,
                viaRelay = it.viaRelay,
                rttMs = it.rttMs,
                estBps = it.estBps,
                lossRate = it.lossRate,
                current = it.current,
                ageSecs = it.ageSecs
            )
        }

/** 估速 humanize(bps → Mbps/Kbps;未测得「—」,与 PC ChannelPanel 同口径) */
fun formatEstBps(bps: ULong?): String = when {
    bps == null || bps == 0uL -> "—"
    bps >= 1_000_000uL -> String.format("%.1f Mbps", bps.toDouble() / 1_000_000.0)
    bps >= 1_000uL -> "${bps / 1_000uL} Kbps"
    else -> "$bps bps"
}

/** 最近探测时间 age(秒 → 刚刚/n秒前/n分前,与 PC ChannelPanel 同口径) */
fun formatAge(ageSecs: ULong): String = when {
    ageSecs < 5uL -> "刚刚"
    ageSecs < 60uL -> "${ageSecs}s前"
    else -> "${ageSecs / 60uL}分前"
}

/**
 * Device UI model
 */
data class DeviceUi(
    val fingerprint: String,
    val name: String,
    val addr: String,
    val online: Boolean,
    val connected: Boolean,
    val viaRelay: Boolean,
    /** M3c T1:通道标签(null=通道表无记录 → 卡片显示「未知」) */
    val channel: ChannelUi? = null,
    /** M3c T3:强制走中继开关在位(卡片角标+⋮菜单勾选态) */
    val forceRelay: Boolean = false
)

/**
 * IPv4 private/loopback detection - checks 192.168.x.x, 10.x.x.x, 172.16-31.x.x, 127.x.x.x
 * Returns false for public IPs and CGNAT ranges (e.g. 100.x.x.x cellular)
 */
fun isPrivateLanIp(ip: String): Boolean {
    val parts = ip.split('.')
    if (parts.size != 4) return false
    val a = parts[0].toIntOrNull() ?: return false
    val b = parts[1].toIntOrNull() ?: return false
    return when {
        a == 10 || a == 127 -> true
        a == 192 && b == 168 -> true
        a == 172 && b in 16..31 -> true
        else -> false
    }
}

/**
 * Main UI state for devices screen
 */
data class DevicesUiState(
    val myFingerprint: String = "",
    val hidden: Boolean = false,
    val devices: List<DeviceUi> = emptyList(),
    val pairing: PairingUiState? = null,
    val loading: Boolean = false,
    val consentTimeoutSecs: Long = 60L,
    val error: String? = null,
    val localIp: String? = null,
    val deviceName: String = "",
    val manualProbe: ManualProbeUiState? = null
)
