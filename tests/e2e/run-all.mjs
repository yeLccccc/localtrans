// run-all —— 一键全场景 + 出口条件②核对报告（RELEASE.md §二.2）
// 用法：node run-all.mjs [--quick] [--only=scene1,scene2]
//   --quick  跳过长稳/慢场景（发版判定仍需全量）
// 逐场景串跑（互斥资源：双PC/手机/端口），单场景失败不阻断后续，
// 末尾产出汇总报告：每场景 PASS/FAIL + 证据目录 + 出口条件②核对表。
import { spawnSync } from 'node:child_process';
import { writeFileSync, mkdirSync, existsSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { loadTargets } from './lib/config.mjs';
import { fileURLToPath } from 'node:url';

const E2E = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const quick = args.includes('--quick');
const onlyIdx = args.findIndex((a) => a.startsWith('--only='));
const only = onlyIdx >= 0 ? args[onlyIdx].split('=')[1].split(',') : null;

// 场景注册表：[文件名, 标签, {quick?:跳过档}]
// 新场景落地后在此登记（M1: l3-reset/cold-start/relay-path 已含在列）
const SCENES = [
  ['api-acceptance.mjs', '全能力验收', {}],
  ['pc-pc-transfer.mjs', 'PC-PC真传输', {}],
  ['l3-reset.mjs', 'L3出厂重置', {}],
  ['cold-start.mjs', '零信任冷启动', {}],
  ['relay-path.mjs', '中继路径', {}],
  ['night-browse-batch.mjs', '浏览批次+排序', {}],
  ['night-android-transfer.mjs', '跨端传输(推送段)', {}],
  ['android-pull-batch.mjs', '安卓批量拉取', {}],
  // M3a T4：连接记忆自动重连——待 huss_laptop 恢复执行；设备不可达时场景内条件跳过(exit 0)
  ['connect-memory.mjs', '连接记忆自动重连(待设备恢复)', {}],
  // M3a T3：名片粘贴添加→发现→配对——待 huss_laptop 恢复执行；同上条件跳过
  ['card-exchange.mjs', '名片互加发现配对(待设备恢复)', {}],
  // M3a T5：TrustBroken 移除信任即时断连降级——待 huss_laptop 恢复执行；同上条件跳过
  ['trust-broken.mjs', 'TrustBroken移除信任通知(待设备恢复)', {}],
  // M3b T4：探测选路通道记录端到端（list_channels 只读观测面）——任一 PC 不可达时场景内条件跳过(exit 0)
  ['routing-probe.mjs', '探测选路通道记录(条件跳过)', {}],
  // M3c T3：强制走中继开关(命令层拦截决策+持久化)+T2 通道面板一致性——任一 PC 不可达时条件跳过(exit 0)
  ['force-relay.mjs', '强制走中继+通道面板(条件跳过)', {}],
  // ---- V1 计划卡 T1/T2 手工清单场景化（2026-09-08）----
  // Task12 核对单 #2：强杀(taskkill)→重启→interrupted 带元数据+续传可用——PC 单机可跑
  ['kill-restart-recovery.mjs', '强杀重启续传恢复(PC单机)', {}],
  // Task12 核对单 #7：位图全真 parts → 重启 failed"完整性存疑"+目录被清——PC 单机可跑
  ['parts-deleted-integrity.mjs', 'parts完整性存疑failed(PC单机)', {}],
  // 安卓手工清单通知项：接收完成通知 dumpsys 硬断言+深链软断言——PC/手机不可达时条件跳过(exit 0)
  ['notify-deeplink.mjs', '接收通知+深链跳转(条件跳过)', {}],
  // 安卓手工清单 #8/#9 + Task12 #9：offer 拒绝/超时/完成 toast——任一 PC 不可达时条件跳过(exit 0)
  ['offer-deny-timeout.mjs', 'offer拒绝超时+完成toast(条件跳过)', {}],
  // 安卓手工清单 #11：手机远程重命名桌面文件——PC/手机不可达时条件跳过(exit 0)
  ['remote-rename.mjs', '手机远程重命名(条件跳过)', {}],
  // P1 打磨卡 T3：速度曲线验收(500MB 推送全程采样 UI 速度,断言无尖刺/爬升/归零)
  // ——对端自适应(laptop 优先,回退 phone);两对端不可达时场景内条件跳过(exit 0)
  ['speed-curve.mjs', '速度曲线平滑验收(条件跳过)', {}],
  // P2 打磨卡 T3：配对失败矩阵(错码重输/对端拒绝/同意门超时/错码3次冷却)
  // ——PC↔手机真机;laptop 不可达遗留双 PC 版;PC/手机不可达时条件跳过(exit 0)
  ['pairing-matrix.mjs', '配对失败矩阵(条件跳过)', {}],
  // ---- P4 性能基线卡（2026-09-09）：perf 单独标签,quick=false(发版全量必跑) ----
  // T1+T2 吞吐基线:PC→手机 WiFi 实测口径(1GB×3 中位+200×10KB 批量)——手机不可达条件跳过(exit 0);
  //   中继口径条件段不部署中继(留双机遗留);慢 WiFi(~3MB/s)下全长 ~25min→timeoutMin 放宽
  ['perf-baseline.mjs', '性能基线:吞吐+批量(条件跳过)', { quick: false, timeoutMin: 45 }],
  // T3 前端渲染:transfers.json 预置 1000 done 卡,展开首屏 <2s 断言——PC 单机
  ['perf-render.mjs', '性能基线:前端1000卡渲染(PC单机)', { quick: false }],
  // T4 空闲足迹:无传输 5 分钟,每 30s 采样进程 CPU,均值 <2% 断言——PC 单机
  ['perf-idle.mjs', '性能基线:空闲足迹5min(PC单机)', { quick: false }],
];

const stamp = new Date().toISOString().replace(/[:T]/g, '-').slice(0, 19);
const outDir = join(E2E, 'reports', `run-all-${stamp}`);
mkdirSync(outDir, { recursive: true });

// 双机可达性预检：不可达时双机场景标记 SKIP(run-all 链式下前站故障不连坐;
// LT_SKIP=force 可强制全跑以暴露真实失败)。单机场景(PC-only)不受影响。
// 注意:targets.local.yaml 是 host/port 平面结构(无 api 字段)——拼 URL 用 host:port
const pcUrl = (t) => `http://${t.host}:${t.port}`;
const pcReachable = await fetch(pcUrl(loadTargets().huss_pc) + '/api/health', { signal: AbortSignal.timeout(4000) })
  .then((r) => r.ok).catch(() => false);
const lapReachable = await fetch(pcUrl(loadTargets().huss_laptop) + '/api/health', { signal: AbortSignal.timeout(4000) })
  .then((r) => r.ok).catch(() => false);
const force = process.env.LT_SKIP === 'force';
const needsBoth = ['api-acceptance.mjs','pc-pc-transfer.mjs','l3-reset.mjs','cold-start.mjs',
                   'relay-path.mjs','night-android-transfer.mjs','android-pull-batch.mjs',
                   'night-browse-batch.mjs'];
console.log(`预检: huss_pc=${pcReachable ? 'UP' : 'DOWN'} huss_laptop=${lapReachable ? 'UP' : 'DOWN'}${force ? ' (force)' : ''}`);
function sceneSkip(file) {
  if (force) return false;
  if (!pcReachable && needsBoth.includes(file)) return true;
  if (!lapReachable && needsBoth.includes(file)) return true;
  return false;
}

console.log(`== run-all: ${SCENES.length} 场景 ==`);
const results = [];
for (const [file, label, opts] of SCENES) {
  if (quick && opts.quick) { results.push({ file, label, status: 'SKIP' }); continue; }
  if (only && !only.includes(file)) { continue; }
  if (sceneSkip(file)) {
    results.push({ file, label, status: 'SKIP', tail: '双机预检不可达' });
    console.log(`[${label}] SKIP(双机预检不可达)`);
    continue;
  }
  if (!existsSync(join(E2E, 'scenarios', file))) {
    results.push({ file, label, status: 'MISSING' });
    console.log(`[${label}] 场景文件缺失，跳过`);
    continue;
  }
  const t0 = Date.now();
  console.log(`\n▶ ${label} (${file})`);
  const r = spawnSync('node', [join('scenarios', file)], {
    cwd: E2E, stdio: ['ignore', 'pipe', 'pipe'],
    // perf-baseline(1GB×3 手机 WiFi)在慢链路(~3MB/s)下 ~25min——perf 场景放宽到 45min
    timeout: (opts.timeoutMin ?? 20) * 60_000, env: process.env,
    encoding: 'utf8',
  });
  const ok = r.status === 0;
  const tail = ((r.stdout || '') + (r.stderr || '')).split('\n').filter(Boolean).slice(-5).join('\n');
  results.push({ file, label, status: ok ? 'PASS' : 'FAIL', secs: Math.round((Date.now() - t0) / 1000), tail });
  console.log(`  ${ok ? '✓ PASS' : '✗ FAIL'} (${results.at(-1).secs}s)`);
}

// 最新报告目录探测（各场景自己落 reports/<场景>-<时间戳>/）
const latestReport = (file) => {
  const base = file.replace('.mjs', '');
  const dirs = readdirSync(join(E2E, 'reports')).filter((d) => d.startsWith(base)).sort();
  return dirs.length ? `reports/${dirs.at(-1)}` : '';
};

const pass = results.filter((r) => r.status === 'PASS').length;
const fail = results.filter((r) => r.status === 'FAIL').length;
const skip = results.filter((r) => r.status === 'SKIP' || r.status === 'MISSING').length;

// ---- 出口条件②核对表（RELEASE.md §二.2）----
// 三端门禁由 CI/开发者另行执行，这里核对场景面。
const gates = {
  '全能力验收(协议/观测/UI/传输控制)': results.find((r) => r.file === 'api-acceptance.mjs')?.status,
  'PC-PC 真实传输': results.find((r) => r.file === 'pc-pc-transfer.mjs')?.status,
  'L3 出厂重置': results.find((r) => r.file === 'l3-reset.mjs')?.status,
  '零信任冷启动(配对+传输)': results.find((r) => r.file === 'cold-start.mjs')?.status,
  '中继路径': results.find((r) => r.file === 'relay-path.mjs')?.status,
  '浏览批次/排序': results.find((r) => r.file === 'night-browse-batch.mjs')?.status,
  '跨端传输': results.find((r) => r.file === 'night-android-transfer.mjs')?.status,
  // V1 补齐（计划卡 T3：发版判定完整性检查覆盖新场景面）
  '强杀重启续传恢复(V1)': results.find((r) => r.file === 'kill-restart-recovery.mjs')?.status,
  'parts 完整性存疑 failed(V1)': results.find((r) => r.file === 'parts-deleted-integrity.mjs')?.status,
};
const gateLines = Object.entries(gates).map(([k, v]) => `| ${k} | ${v ?? '未登记'} |`).join('\n');

const md = `# run-all 汇总 — ${stamp}

**结果: ${pass} PASS / ${fail} FAIL / ${skip} SKIP** （quick=${quick}）

## 出口条件②核对表（RELEASE.md）

| 检查项 | 状态 |
|---|---|
${gateLines}

> 三端门禁（core/shell/ui cargo+npm）由开发者/CI 另行执行；
> 本表仅覆盖场景面。全 PASS + 门禁绿 = 出口条件②满足。

## 明细

| 场景 | 标签 | 状态 | 耗时 | 证据 |
|---|---|---|---|---|
${results.map((r) => `| ${r.file} | ${r.label} | ${r.status} | ${r.secs ?? '-'}s | ${latestReport(r.file)} |`).join('\n')}

${results.filter((r) => r.status === 'FAIL').map((r) => `## ${r.label} 失败尾部\n\`\`\`\n${r.tail}\n\`\`\``).join('\n\n')}
`;
const reportPath = join(outDir, 'report.md');
writeFileSync(reportPath, md, 'utf8');
console.log(`\n== 汇总: ${pass} PASS / ${fail} FAIL / ${skip} SKIP ==`);
console.log(`报告: ${reportPath}`);
process.exitCode = fail > 0 ? 1 : 0;
