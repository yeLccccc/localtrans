package com.localtrans.app.ui.transfers

/**
 * UI State for Transfers Screen
 */
data class TransfersUiState(
    val transfers: List<TransferUi> = emptyList(),
    val offerSheet: OfferSheetUi? = null
)

/**
 * 父卡子文件行(桌面 ChildDto 等价;jobId 为引擎侧字符串 id)
 */
data class ChildUi(
    val jobId: String,
    val name: String,
    val total: Long,
    val done: Long,
    val state: String
)

/**
 * Transfer item UI model
 */
data class TransferUi(
    val jobId: Long,
    val name: String,
    val total: Long,
    val done: Long,
    val state: String,
    val speedBps: Long,
    val peer: String,
    val direction: String, // "tx" or "rx"
    val localRole: String, // "sender" or "receiver"
    val progressPercent: Int,
    val etaSecs: Long,
    val failReason: String,
    val localPath: String? = null,
    val remoteDone: Long = 0,
    val instant: Boolean = false,
    // M2 T5 契约消费(FFI T1 已铺):排队位次/批次/子文件/parts/时间戳/恢复参数
    val queuePos: Int? = null,
    val batchId: String? = null,
    val children: List<ChildUi> = emptyList(),
    val partsId: String? = null,
    val startedAtMs: Long? = null,
    val finishedAtMs: Long? = null,
    val sourcePath: String? = null
)

/**
 * Offer sheet UI model
 */
data class OfferSheetUi(
    val jobId: Long,
    val peerName: String,
    val fileCount: Int,
    val totalSize: Long,
    val deadlineEpochMs: Long,
    val remainingSecs: Int
)
