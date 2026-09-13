/**
 * 传输任务 Store
 * 管理传输任务列表，4Hz 批量更新
 */

import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import type { TransferDto, TransferAction, ResumeJobInfo, DiskJobDto } from '../types'
import { api, onTransferProgress } from '../api'
import { SpeedSmoother } from '../lib/speedSmooth'

const TERMINAL_STATES = new Set(['done', 'failed', 'interrupted'])

export const useTransfersStore = defineStore('transfers', () => {
  // 传输任务列表（使用 shallowRef 避免深层响应式）
  const transfers = shallowRef<TransferDto[]>([])

  // R2 速度平滑:每任务一个平滑器。Rust 4Hz 泵的原始 speed_bps + done 进表前
  // 过进度导数+EMA(τ=2s)+1s 滑窗尖峰抑制+500ms 显示节流;
  // ETA 在组件层用平滑后速度计算。
  const speedSmoothers = new Map<string, SpeedSmoother>()

  /**
   * 速度显示化:jobs 里的 speed_bps 替换为平滑后显示值。
   * 终态卡清平滑器并强制归零(结束归零,不留长尾)。
   */
  function applySpeedDisplay(jobs: TransferDto[]): TransferDto[] {
    const now = Date.now()
    return jobs.map((j) => {
      if (TERMINAL_STATES.has(j.state)) {
        speedSmoothers.delete(j.job_id)
        return j.speed_bps === 0 ? j : { ...j, speed_bps: 0 }
      }
      let sm = speedSmoothers.get(j.job_id)
      if (!sm) {
        sm = new SpeedSmoother()
        speedSmoothers.set(j.job_id, sm)
      }
      return { ...j, speed_bps: sm.push(j.speed_bps, now, j.done) }
    })
  }

  // 可恢复任务列表
  const resumeJobs = ref<ResumeJobInfo[]>([])

  // 加载状态
  const loading = ref(false)

  // 错误信息
  const error = ref<string | null>(null)

  // 用于批量更新的定时器
  let updateTimer: number | null = null
  let pendingUpdate: TransferDto[] | null = null

  // 记录任务的原始请求参数（用于失败重试）
  const lastRequest = ref<Map<string, { type: 'pull' | 'push' | 'push-rel'; fp: string; share_id?: string; path?: string; paths?: string[]; items?: [string, string][] }>>(new Map())

  /**
   * 刷新传输任务列表
   */
  async function refreshTransfers() {
    loading.value = true
    error.value = null
    try {
      transfers.value = applySpeedDisplay(await api.transfers.list())
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to load transfers:', e)
    } finally {
      loading.value = false
    }
  }

  /**
   * 刷新可恢复任务列表
   */
  async function refreshResumeJobs() {
    try {
      resumeJobs.value = await api.transfers.pendingResumeJobs()
    } catch (e) {
      console.error('Failed to load resume jobs:', e)
    }
  }

  /**
   * 记录拉取任务请求参数
   */
  function recordPullRequest(job_id: string, fp: string, share_id: string, path: string) {
    lastRequest.value.set(job_id, { type: 'pull', fp, share_id, path })
  }

  /**
   * 记录推送任务请求参数
   */
  function recordPushRequest(job_id: string, fp: string, paths: string[]) {
    lastRequest.value.set(job_id, { type: 'push', fp, paths })
  }

  /**
   * v0.5.0 记录推送任务请求参数（带相对目录）
   */
  function recordPushRelRequest(job_id: string, fp: string, items: [string, string][]) {
    lastRequest.value.set(job_id, { type: 'push-rel', fp, items })
  }

  /**
   * 传输任务控制
   */
  async function transferAction(job_id: string, action: TransferAction) {
    try {
      await api.browse.transferAction(job_id, action)

      // 更新本地状态
      const index = transfers.value.findIndex((t) => t.job_id === job_id)
      if (index !== -1) {
        const updated = { ...transfers.value[index] }
        if (action === 'pause') {
          updated.state = 'paused'
        } else if (action === 'cancel') {
          updated.state = 'failed'
        } else if (action === 'resume') {
          updated.state = 'active'
        }
        transfers.value = [
          ...transfers.value.slice(0, index),
          updated,
          ...transfers.value.slice(index + 1),
        ]
      }
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to control transfer:', e)
      throw e
    }
  }

  /**
   * 失败重试
   */
  async function retryTransfer(job_id: string) {
    const request = lastRequest.value.get(job_id)
    if (!request) {
      throw new Error('未找到任务原始请求参数，无法重试')
    }

    try {
      if (request.type === 'pull') {
        await api.browse.startDownload(request.fp, request.share_id!, request.path!)
      } else if (request.type === 'push') {
        await api.browse.pushFiles(request.fp, request.paths!)
      } else if (request.type === 'push-rel') {
        await api.browse.pushFilesRel(request.fp, request.items!)
      }
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to retry transfer:', e)
      throw e
    }
  }

  /**
   * 恢复待处理任务
   */
  async function resumePending(job_id: string) {
    try {
      await api.transfers.resumePending(job_id)

      // 从可恢复列表中移除
      resumeJobs.value = resumeJobs.value.filter(([id]) => id !== job_id)

      // 刷新传输列表
      await refreshTransfers()
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to resume pending job:', e)
      throw e
    }
  }

  /**
   * 批量更新传输列表（4Hz 防抖）
   */
  function scheduleUpdate(newTransfers: TransferDto[]) {
    if (updateTimer !== null) {
      // 已有定时器在运行，保存待更新数据
      pendingUpdate = newTransfers
      return
    }

    // 立即更新
    transfers.value = newTransfers
    pendingUpdate = null

    // 启动防抖定时器（250ms = 4Hz）
    updateTimer = window.setTimeout(() => {
      // 定时器触发时，如果有待更新数据，进行最终更新
      if (pendingUpdate !== null) {
        transfers.value = pendingUpdate
        pendingUpdate = null
      }
      updateTimer = null
    }, 250)
  }

  /**
   * 初始化事件监听
   */
  function setupEventListeners() {
    // 监听传输进度更新（4Hz 批量更新;速度字段先过平滑器再进表）
    onTransferProgress((progress) => {
      scheduleUpdate(applySpeedDisplay(progress.jobs))
    })
  }

  /**
   * 清除已完成的任务
   */
  async function clearCompleted(): Promise<number> {
    const n = await api.transfers.clearCompleted()
    await refreshTransfers()
    return n
  }

  /**
   * 删除传输任务(两级:view 仅移除视图由 4Hz 事件收敛,destroy 乐观过滤)
   */
  async function removeTransfer(job_id: string, level: 'view' | 'destroy'): Promise<void> {
    await api.transfers.removeTransfer(job_id, level)
    if (level === 'destroy') {
      transfers.value = transfers.value.filter(t => t.job_id !== job_id)
    }
  }

  // 磁盘历史任务列表(历史记录区)
  const diskJobs = ref<DiskJobDto[]>([])

  /**
   * 刷新磁盘历史任务列表(打开历史区时拉取)
   */
  async function refreshDiskJobs() {
    try {
      diskJobs.value = await api.transfers.listDiskJobs()
    } catch (e) {
      console.error('Failed to load disk jobs:', e)
    }
  }

  /**
   * 恢复磁盘历史任务到视图(操作后刷新历史)
   */
  async function restoreDiskJob(job_id: string) {
    await api.transfers.restoreDiskJob(job_id)
    await refreshDiskJobs()
  }

  /**
   * 彻底销毁磁盘历史任务(操作后刷新历史)
   */
  async function destroyDiskJob(job_id: string) {
    await api.transfers.destroyDiskJob(job_id)
    await refreshDiskJobs()
  }

  /**
   * 限制传输并发流数
   */
  async function throttleTransfer(job_id: string, maxStreams: number): Promise<void> {
    await api.transfers.transferThrottle(job_id, maxStreams)
  }

  /**
   * 初始化 store
   */
  async function initialize() {
    setupEventListeners()
    await refreshTransfers()
    await refreshResumeJobs()
  }

  /**
   * Task M3: 测试快照（spec §7.1.6 Pinia 侧）。
   * 返回纯 JSON 可序列化对象：JSON 往返剥响应式/函数/undefined；
   * lastRequest（失败重试的请求参数 Map）是内部簿记不含断言价值，不进快照。
   * 契约见 docs/contracts/test-api.md §5.3。
   */
  function toTestSnapshot() {
    return JSON.parse(
      JSON.stringify({
        schemaVersion: 1,
        transfers: transfers.value,
        resumeJobs: resumeJobs.value,
        diskJobs: diskJobs.value,
        loading: loading.value,
        error: error.value,
      }),
    )
  }

  return {
    // 状态
    transfers,
    resumeJobs,
    loading,
    error,
    lastRequest,
    diskJobs,

    // 方法
    refreshTransfers,
    refreshResumeJobs,
    transferAction,
    resumePending,
    recordPullRequest,
    recordPushRequest,
    recordPushRelRequest,
    retryTransfer,
    clearCompleted,
    removeTransfer,
    refreshDiskJobs,
    restoreDiskJob,
    destroyDiskJob,
    throttleTransfer,
    initialize,
    toTestSnapshot,
  }
})
