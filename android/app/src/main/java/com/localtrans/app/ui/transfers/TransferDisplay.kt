package com.localtrans.app.ui.transfers

/**
 * 传输卡展示纯函数(M2 T5,移植桌面 ui/src/lib/transferDisplay.ts;
 * 逻辑收拢在此,Composable 只做绑定,单测直接覆盖)。
 */

/** 对端展示名数据源(指纹/广播名/别名;Android 侧 DeviceDto 无 alias,恒空串) */
data class PeerNameSource(
    val fingerprint: String,
    val name: String,
    val alias: String
)

/** 指纹缩写:aa9988..ff12 */
fun fingerprintAbbr(fp: String): String =
    if (fp.length > 12) fp.take(6) + "..." + fp.takeLast(6) else fp

/**
 * 对端展示名 —— 别名 > 广播名 > 指纹缩写(指纹匹配不区分大小写)
 */
fun peerDisplayName(peerHex: String, peers: List<PeerNameSource>): String {
    val lower = peerHex.lowercase()
    val peer = peers.find { it.fingerprint.lowercase() == lower }
    if (peer != null) {
        if (peer.alias.isNotEmpty()) return peer.alias
        if (peer.name.isNotEmpty()) return peer.name
    }
    return fingerprintAbbr(peerHex)
}

/** 终态集合(done/failed/interrupted 进历史区) */
val TERMINAL_STATES = setOf("done", "failed", "interrupted")

/** 活动态集合(active/pending/paused/cancelling 留活动区) */
val ACTIVE_STATES = setOf("active", "pending", "paused", "cancelling")

fun isTerminalState(state: String): Boolean = state in TERMINAL_STATES

/** 单进度条取值(R1:只渲染一条进度条,remote_done 是发送方的确认度信号) */
data class ProgressPair(
    /** 主进度字节(发送方=remoteDone 已确认,接收方=done) */
    val mainDone: Long,
    /** 本机已发字节(发送方=done,接收方=done) */
    val sentDone: Long,
    /** 网络积压百分比 (done-remoteDone)/total*100,仅发送方有意义 */
    val backlogPct: Double
)

/**
 * 发送方(source-push)主进度=remoteDone(对端已确认接收),本机已发=done;
 * 接收方主进度=done。
 * 容错(旧任务无 remote_done 镜像):done>0 且 remoteDone==0 且 total>0 且非终态
 * 视为"无镜像数据",退回单进度显示(mainDone=done),绝不显示 0% 误导。
 */
fun progressPair(t: TransferUi): ProgressPair {
    if (t.localRole != "source-push") {
        return ProgressPair(mainDone = t.done, sentDone = t.done, backlogPct = 0.0)
    }
    val remote = t.remoteDone
    val noMirrorData = t.done > 0 && remote == 0L && t.total > 0 && !isTerminalState(t.state)
    val mainDone = if (noMirrorData) t.done else remote
    val backlogPct = if (t.total > 0 && !noMirrorData) {
        (t.done - remote).toDouble() / t.total * 100.0
    } else 0.0
    return ProgressPair(mainDone = mainDone, sentDone = t.done, backlogPct = maxOf(0.0, backlogPct))
}

/**
 * 发送方展示态(R1 文案态,不画第二条进度条/角标):
 * - awaiting-confirm:满格(done>=total>0)但对端未确认满(remoteDone<total)
 * - backlog:积压 >20%
 * 优先级:满格未确认优先。两者在 UI 上同现"等待对方确认"文案。
 */
enum class SenderDisplayState { NORMAL, AWAITING_CONFIRM, BACKLOG }

fun senderDisplayState(t: TransferUi): SenderDisplayState {
    val remote = t.remoteDone
    if (t.total > 0 && t.done >= t.total && remote < t.total) {
        return SenderDisplayState.AWAITING_CONFIRM
    }
    val pair = progressPair(t.copy(state = if (t.state.isEmpty()) "active" else t.state))
    if (t.localRole == "source-push" && pair.backlogPct > 20) return SenderDisplayState.BACKLOG
    return SenderDisplayState.NORMAL
}

/** 发送方是否应显示"等待对方确认"文案态(非终态才有意义) */
fun isAwaitingConfirmText(t: TransferUi): Boolean {
    if (t.localRole != "source-push") return false
    if (t.state !in setOf("active", "paused")) return false
    return senderDisplayState(t) != SenderDisplayState.NORMAL
}

/** 主进度百分比 0..100 */
fun mainPercent(t: TransferUi): Double {
    if (t.total <= 0L) return 0.0
    val main = progressPair(t).mainDone
    return (main.toDouble() / t.total * 100.0).coerceIn(0.0, 100.0)
}

/**
 * 排队位次文案:queuePos 非空 → "排队中 · 第 N 位";
 * 否则回退方向固定文案(pull=连对端调度,push=等人接收)
 */
fun queueText(queuePos: Int?, direction: String): String {
    if (queuePos != null) return "排队中 · 第 $queuePos 位"
    return if (direction == "pull") "排队中，连接对端..." else "等待对方接收..."
}

/** 活动区状态权重:active > cancelling > pending > paused */
private val ACTIVE_RANK = mapOf("active" to 0, "cancelling" to 1, "pending" to 2, "paused" to 3)

/** 分区结果 */
data class ActiveHistory(val active: List<TransferUi>, val history: List<TransferUi>)

/**
 * 传输列表分区(桌面 splitActiveHistory 同款):
 * active = active/pending/paused/cancelling,排序 active>cancelling>pending(queue_pos 升序,null 靠后)>paused,
 * 同级按 startedAtMs 升序(老的在前,null 靠后);
 * history = done/failed/interrupted,按 finishedAtMs 降序(新的在前,null 靠后)。
 */
fun splitActiveHistory(jobs: List<TransferUi>): ActiveHistory {
    val active = jobs.filter { it.state in ACTIVE_STATES }.toMutableList()
    val history = jobs.filter { it.state in TERMINAL_STATES }.toMutableList()
    active.sortWith(
        compareBy<TransferUi> { ACTIVE_RANK[it.state] ?: 99 }
            .thenComparator { a, b ->
                if ((ACTIVE_RANK[a.state] ?: 99) == 2) {
                    // pending:queue_pos 升序,null 靠后
                    val (qa, qb) = a.queuePos to b.queuePos
                    when {
                        qa != null && qb != null && qa != qb -> qa - qb
                        qa != null -> -1
                        qb != null -> 1
                        else -> 0
                    }
                } else 0
            }
            .thenComparator { a, b -> compareNullableAsc(a.startedAtMs, b.startedAtMs) }
    )
    history.sortWith(
        compareByDescending<TransferUi> { it.finishedAtMs != null }
            .thenComparator { a, b -> -compareNullableAsc(a.finishedAtMs, b.finishedAtMs) }
    )
    return ActiveHistory(active = active, history = history)
}

/** null 靠后的升序比较(桌面 null-last 惯例) */
private fun compareNullableAsc(a: Long?, b: Long?): Int = when {
    a != null && b != null -> a.compareTo(b)
    a == null && b != null -> 1
    a != null && b == null -> -1
    else -> 0
}

/** 已用时长文案(桌面 formatDuration 秒口径):45s / 3m20s / 2h5m */
fun formatElapsed(secs: Long): String = when {
    secs < 60 -> "${secs}s"
    secs < 3600 -> "${secs / 60}m${secs % 60}s"
    else -> "${secs / 3600}h${(secs % 3600) / 60}m"
}

/** 已用时长秒数:startedAtMs 起算,终态冻结在 finishedAtMs(null=未开始) */
fun elapsedSeconds(t: TransferUi, nowMs: Long): Long? {
    val start = t.startedAtMs ?: return null
    val end = t.finishedAtMs ?: nowMs
    return maxOf(0L, (end - start) / 1000)
}

/** 子项状态文案(桌面 childStateText 同表) */
fun childStateText(state: String): String = when (state) {
    "pending" -> "等待中"
    "active" -> "进行中"
    "paused" -> "已暂停"
    "done" -> "完成"
    "failed" -> "失败"
    "interrupted" -> "已中断"
    else -> state
}

/** 角色文案(桌面 roleText 同表) */
fun roleText(localRole: String): String = when (localRole) {
    "destination" -> "接收方"
    "source-push" -> "推送方"
    "source-pull" -> "被取方"
    else -> "传输"
}
