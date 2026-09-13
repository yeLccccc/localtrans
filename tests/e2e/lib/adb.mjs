// adb.mjs — Android adb 工具层（M7，spec §7.3 / FR-10/FR-11）。
// Node ESM，零第三方依赖：child_process 直调 adb。
// 约定：
//  - serial 取 targets.local.yaml android.serial（可经 opts.serial 覆盖）；
//  - adb 二进制解析顺序：LOCALTRANS_ADB env → targets android.adb →
//    android/local.properties sdk.dir 推导 platform-tools/adb.exe → PATH adb
//    （老 adb(≤1.0.31) 无 exec-out，优先用 SDK platform-tools 版本）；
//  - 每次交互前新鲜 dump（NFR-6，缓存禁用）；
//  - 中文输入不支持（ADBKeyboard 未装），明确报错不静默。
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadTargets } from './config.mjs';

export const PACKAGE = 'com.localtrans.app';
export const MAIN_ACTIVITY = `${PACKAGE}/.MainActivity`;

/** 解析 adb 可执行文件路径。 */
export function resolveAdbPath(explicit) {
  if (explicit) return explicit;
  if (process.env.LOCALTRANS_ADB) return process.env.LOCALTRANS_ADB;
  let yamlAdb;
  try {
    yamlAdb = loadTargets().huss_phone.adb;
  } catch { /* yaml 不在也不阻断 */ }
  if (yamlAdb) return yamlAdb;
  try {
    // android/local.properties（gitignored）的 sdk.dir → platform-tools
    const lp = fileURLToPath(new URL('../../../android/local.properties', import.meta.url));
    if (existsSync(lp)) {
      const m = readFileSync(lp, 'utf8').match(/^sdk\.dir\s*=\s*(.+)$/m);
      if (m) {
        const cand = `${m[1].trim().replace(/[\\/]$/, '')}/platform-tools/adb.exe`;
        if (existsSync(cand)) return cand;
      }
    }
  } catch { /* 尽力而为 */ }
  return 'adb';
}

/**
 * 创建绑定一台设备的 adb 通道。
 * @param {{serial?:string, adbPath?:string}} [opts]
 */
export function createAdbChannel(opts = {}) {
  let serial = opts.serial;
  if (!serial) {
    const targets = loadTargets();
    serial = targets.huss_phone.serial;
    if (!serial) throw new Error('targets.local.yaml 缺少 android.serial');
  }
  const adbPath = resolveAdbPath(opts.adbPath);

  /** 执行 adb 子命令（设备级自动加 -s serial）。 */
  const run = (args, { timeoutMs = 30000, encoding = 'utf8', maxBuffer } = {}) =>
    execFileSync(adbPath, ['-s', serial, ...args], {
      timeout: timeoutMs,
      encoding,
      maxBuffer: maxBuffer || 32 * 1024 * 1024,
      windowsHide: true,
    });

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // ===== 部署/生命周期 =====

  /**
   * 安装（覆盖）APK。定制 ROM（MIUI 等）安装时会在手机端弹确认框，
   * 默认并发监视 dump 并自动点掉（仅点系统安装器包名的"继续安装/确定"按钮，
   * autoConfirm=false 关闭）。
   */
  const install = async (apkPath, { timeoutMs = 240000, autoConfirm = true } = {}) => {
    const child = spawn(adbPath, ['-s', serial, 'install', '-r', apkPath], {
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
    let out = '';
    child.stdout.on('data', (d) => { out += d; });
    child.stderr.on('data', (d) => { out += d; });

    const INSTALLER_PKGS = [
      'com.miui.securitycenter', 'com.android.packageinstaller',
      'com.google.android.packageinstaller', 'com.android.systemui',
    ];
    if (autoConfirm) {
      (async () => {
        const deadline = Date.now() + timeoutMs;
        while (Date.now() < deadline && child.exitCode === null) {
          await sleep(800);
          if (child.exitCode !== null) return;
          try {
            const els = await dump();
            const btn = els.find((e) => e.clickable
              && INSTALLER_PKGS.includes(e.package)
              && (/继续安装|继续$/.test(e.text) || e.text === '确定' || /install/i.test(e.text)));
            if (btn) run(['shell', 'input', 'tap', String(btn.center[0]), String(btn.center[1])]);
          } catch { /* dump 失败重试 */ }
        }
      })();
    }

    const code = await new Promise((resolve) => {
      const timer = setTimeout(() => child.kill(), timeoutMs);
      child.on('exit', (c) => { clearTimeout(timer); resolve(c); });
    });
    if (code !== 0 || !/Success/.test(out)) {
      throw new Error(`install 失败(exit=${code}): ${out.trim().slice(0, 300)}`);
    }
    return out.trim();
  };

  /** 启动主 Activity（-W 等待完成启动）。 */
  const launch = () => {
    const out = run(['shell', 'am', 'start', '-W', '-n', MAIN_ACTIVITY]);
    if (!/Status:\s*ok/i.test(out)) throw new Error(`launch 失败: ${String(out).trim()}`);
    return String(out).trim();
  };

  /** 强停应用。 */
  const forceStop = () => {
    run(['shell', 'am', 'force-stop', PACKAGE]);
  };

  /** 清空 logcat 缓冲（launch 前调用，避免匹配到上一轮横幅）。 */
  const clearLogcat = () => {
    run(['logcat', '-c']);
  };

  // ===== 观测：UI dump =====

  const decodeXmlEntities = (s) => s
    .replace(/&#(\d+);/g, (_, n) => String.fromCodePoint(Number(n)))
    .replace(/&lt;/g, '<').replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"').replace(/&apos;/g, "'")
    .replace(/&amp;/g, '&');

  /**
   * 新鲜 dump（每次调用重新 uiautomator dump，无缓存）。
   * 返回元素数组：{ testTag(resource-id), text, contentDesc, clickable,
   * selected, checkable, checked, className, bounds:[x1,y1,x2,y2], center:[cx,cy] }
   */
  const dump = async () => {
    // 残留的 uiautomator 进程会霸占 UiAutomation 连接，后续 dump 拿到空壳树
    // （密集连发场景实测；失败轮症状=窗口在/resume 但语义树空）——先清场
    try { run(['shell', 'pkill', 'uiautomator']); } catch { /* 无残留 */ }
    const tmp = '/sdcard/lt_e2e_dump.xml';
    run(['shell', 'uiautomator', 'dump', tmp], { timeoutMs: 60000 });
    const xml = run(['shell', 'cat', tmp]);
    const els = [];
    for (const m of String(xml).matchAll(/<node\b[^>]*>/g)) {
      const node = m[0];
      const attr = (name) => {
        const a = node.match(new RegExp(`${name}="([^"]*)"`));
        return a ? decodeXmlEntities(a[1]) : '';
      };
      const b = attr('bounds').match(/\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]/);
      if (!b) continue;
      const [x1, y1, x2, y2] = [Number(b[1]), Number(b[2]), Number(b[3]), Number(b[4])];
      els.push({
        testTag: attr('resource-id'),
        text: attr('text'),
        contentDesc: attr('content-desc'),
        clickable: attr('clickable') === 'true',
        selected: attr('selected') === 'true',
        checked: attr('checked') === 'true',
        scrollable: attr('scrollable') === 'true',
        className: attr('class'),
        package: attr('package'),
        bounds: [x1, y1, x2, y2],
        center: [Math.round((x1 + x2) / 2), Math.round((y1 + y2) / 2)],
      });
    }
    if (els.length === 0) throw new Error(`uiautomator dump 解析出 0 个元素（XML 前 200 字: ${String(xml).slice(0, 200)}）`);
    return els;
  };

  /** dump 摘要（错误消息/日志用）。 */
  const dumpSummary = (els = [], limit = 25) =>
    els.slice(0, limit)
      .map((e) => `[${e.className.split('.').pop()}${e.clickable ? '*' : ''} tag=${e.testTag || '-'} text="${e.text.slice(0, 24)}" cd="${e.contentDesc.slice(0, 16)}" b=${e.bounds.join(',')}]`)
      .join('\n');

  /**
   * 选择器匹配：{ testTag } 精确；{ text } / { contentDesc } 精确。
   * 同名多匹配时优先 clickable（Compose 可点击节点才是 tap 目标）。
   */
  const findAll = (els, sel) => {
    const pred = (e) =>
      (sel.testTag !== undefined && e.testTag === sel.testTag) ||
      (sel.text !== undefined && e.text === sel.text) ||
      (sel.contentDesc !== undefined && e.contentDesc === sel.contentDesc);
    const hits = els.filter(pred);
    return hits.sort((a, b) => Number(b.clickable) - Number(a.clickable));
  };

  const find = (els, sel) => {
    const hits = findAll(els, sel);
    if (hits.length === 0) {
      throw new Error(`元素未找到: ${JSON.stringify(sel)}\n当前 dump（前 ${Math.min(25, els.length)} 元素）:\n${dumpSummary(els)}`);
    }
    return hits[0];
  };

  // ===== 操作：tap / 输入 =====

  /** 原始坐标点击（元素已由调用方定位;tap(sel) 是选择器版,不接受坐标）。 */
  const tapXY = (x, y) => run(['shell', 'input', 'tap', String(Math.round(x)), String(Math.round(y))]);

  /**
   * 点击：新鲜 dump 定位 → input tap 中心坐标。
   * 找不到自动短重试（重 dump，500ms 间隔），超限抛错附 dump 摘要。
   */
  const tap = async (sel, { retries = 2 } = {}) => {
    let lastErr;
    for (let i = 0; i <= retries; i++) {
      const els = await dump();
      try {
        const el = find(els, sel);
        const [cx, cy] = el.center;
        run(['shell', 'input', 'tap', String(cx), String(cy)]);
        return el;
      } catch (e) {
        lastErr = e;
        if (i < retries) await sleep(500);
      }
    }
    throw lastErr;
  };

  /**
   * 文本输入：仅 ASCII（空格转义 %s，shell 元字符加反斜杠转义）。
   * 非 ASCII 抛错——需 ADBKeyboard 专用 IME（未安装，OQ-4 留待后续）。
   * 注意：目标须已聚焦输入框，本函数不负责定位。
   */
  const inputText = (str) => {
    if (!/^[\x20-\x7E]*$/.test(str)) {
      throw new Error(`inputText 不支持非 ASCII（"${str}"）：中文输入需 ADBKeyboard（未安装），见 spec OQ-4`);
    }
    // %s 是 input text 的空格转义；其余 shell 元字符反斜杠转义防 adb 远端 sh 解析
    const escaped = str
      .replace(/ /g, '%s')
      .replace(/[$&;()<>|\\`"']/g, (c) => `\\${c}`);
    run(['shell', 'input', 'text', escaped]);
  };

  // ===== 证据：截图 / 日志 =====

  /** 截图落盘（exec-out 二进制流，校验 PNG 头）。 */
  const screenshot = (localPath) => {
    const buf = run(['exec-out', 'screencap', '-p'], { encoding: 'buffer', timeoutMs: 60000 });
    if (!(buf.length > 8 && buf[0] === 0x89 && buf.slice(1, 4).toString() === 'PNG')) {
      throw new Error(`screenshot 输出非 PNG（${buf.length} 字节，头: ${buf.slice(0, 8).toString('hex')}）`);
    }
    writeFileSync(localPath, buf);
    return { path: localPath, bytes: buf.length };
  };

  /**
   * 拉取 logcat（-d 落盘快照，-v time 带时间戳）。
   * @param {{since?:string, tagPrefix?:string|string[], grep?:string|string[]}} o
   *   since: logcat -T 时间过滤（"MM-DD HH:MM:SS.mmm" 或 "1s ago" 风格）；
   *   tagPrefix: 行内 tag 前缀过滤（JS 侧实现，支持 LT:: 多级前缀）；
   *   grep: 消息子串过滤。
   * 返回（已过滤的）完整文本行。
   */
  const logcat = ({ since, tagPrefix, grep } = {}) => {
    const args = ['logcat', '-d', '-v', 'time'];
    if (since) args.push('-T', since);
    let out = String(run(args, { timeoutMs: 60000 }));
    if (tagPrefix) {
      const prefixes = Array.isArray(tagPrefix) ? tagPrefix : [tagPrefix];
      // -v time 行格式: "MM-DD HH:MM:SS.mmm L/TAG(pid): msg" —— 注意 TAG 与 (pid) 间无空格
      out = out.split('\n').filter((l) => {
        const m = l.match(/\/(\S+)\(\s*(\d+)\): /); // pid 前可有空格(短pid格式)
        if (!m) return false;
        const tag = m[1].split('/').pop(); // 去掉 "I/" 级别前缀
        return prefixes.some((p) => tag === p || tag.startsWith(`${p}::`));
      }).join('\n');
    }
    if (grep) {
      const subs = Array.isArray(grep) ? grep : [grep];
      out = out.split('\n').filter((l) => subs.some((s) => l.includes(s))).join('\n');
    }
    return out;
  };

  /**
   * 轮询等待启动横幅（LT-BANNER，FR-11 就绪信号）。
   * 前提：调用前已 clearLogcat + launch。返回命中的横幅行（含版本/指纹）。
   */
  const waitForBanner = async (timeoutMs = 30000) => {
    const deadline = Date.now() + timeoutMs;
    let last = '';
    while (Date.now() < deadline) {
      const lines = logcat({ tagPrefix: 'LT' }).split('\n').filter((l) => l.includes('LT-BANNER'));
      if (lines.length > 0) return lines[lines.length - 1].trim();
      last = 'logcat 中暂无 LT-BANNER';
      await sleep(1200);
    }
    throw new Error(`waitForBanner 超时（${timeoutMs}ms）：${last}`);
  };

  /** 唤醒屏幕并尝试上滑解锁（无 PIN 锁屏场景；设了 PIN 仍需人工解锁一次）。
   * 息屏时 uiautomator dump 拿不到节点、launch 也不可见——远程操作前必须先唤醒。 */
  /** 坐标滑动（滚动列表用；up=true 向上滚即看下方内容）。 */
  const swipe = (x1, y1, x2, y2, durMs = 300) =>
    run(['shell', 'input', 'swipe', String(x1), String(y1), String(x2), String(y2), String(durMs)]);

  /** 唤醒屏幕并解除遮挡（锁屏->上滑解锁；通知栏->BACK 收起）。
   * 教训（实测踩坑）：无锁屏时盲目上滑会把通知栏拉下来盖住应用——uiautomator
   * 拿到 52 个通知面板节点、0 个 app testTag，页面不可滚动时必现。 */
  const wake = () => {
    run(['shell', 'input', 'keyevent', 'KEYCODE_WAKEUP']);
    try {
      const w = run(['shell', 'dumpsys', 'window'], { timeoutMs: 15000 });
      const locked = /mDreamingLockscreen=true/.test(w);
      const shaded = /mCurrentFocus=Window\{[^}]*\s[^}]*(systemui|shade)/.test(w);
      if (locked) run(['shell', 'input', 'swipe', '540', '1600', '540', '400', '150']);
      else if (shaded) run(['shell', 'input', 'keyevent', '4']); // BACK 收起通知栏
    } catch { /* dumpsys 失败就不做额外动作，仅点亮 */ }
  };

  /** 测试期间保持亮屏（USB 供电时）；恢复传 false。根治：等待类步骤中手机自动息屏
   * 导致后续 uiautomator dump 只剩空壳窗口（实测：banner 等待 30s 内即息屏）。 */
  const setStayOn = (on) => {
    run(['shell', 'svc', 'power', 'stayon', on ? 'usb' : 'false']);
  };

  // ===== L3 出厂重置（spec §7.4.5，T2 固化本周实证流程）=====

  /**
   * 出厂重置：pm clear 清应用数据（信任表/配置/传输记录全清）→ 清 logcat →
   * launch → 轮询 LT-BANNER → MIUI 运行时权限弹窗自动放行（新鲜 dump 找
   * clickable 的"始终允许/仅在使用中允许/允许"逐枚点掉，最多 maxRounds 轮；
   * 连续 idleRounds 轮 dump 无弹窗视为放行结束——弹窗渲染滞后于 banner 的兜底）。
   * @param {{maxRounds?:number, idleRounds?:number}} [opts]
   * @returns {{cleared:string, banner:string, grants:string[]}} pm clear 输出、就绪横幅行、已放行的弹窗文本
   */
  const factoryReset = async ({ maxRounds = 6, idleRounds = 3 } = {}) => {
    const cleared = String(run(['shell', 'pm', 'clear', PACKAGE])).trim();
    if (!/Success/i.test(cleared)) throw new Error(`pm clear 失败: ${cleared}`);
    // pm clear 会收回运行时权限——先 grant 再启动,权限弹窗就不该出现
    // (MIUI 偶发仍弹,自动放行循环兜底;2026-09-07 实证 grant 后依旧弹的
    //  是 MIUI 特有的"始终允许"二次确认,放行循环处理)
    for (const perm of ['android.permission.READ_EXTERNAL_STORAGE',
                        'android.permission.WRITE_EXTERNAL_STORAGE']) {
      try { run(['shell', 'pm', 'grant', PACKAGE, perm]); } catch { /* API 版差异 */ }
    }
    clearLogcat();
    launch();
    // 先放行权限弹窗再等 banner——弹窗挡住时 Compose 不渲染,
    // waitForBanner 会等到超时(MIUI:pm grant 报成功但 granted=false,
    // 必须走 UI"始终允许";2026-09-07 实证)
    const ALLOW_RE = /^(始终允许|仅在使用中允许|允许|仅本次允许)$/;
    const grants = [];
    let idle = 0;
    while (grants.length < maxRounds && idle < idleRounds) {
      let hit;
      try {
        const els = await dump();
        hit = els.find((e) => e.clickable && ALLOW_RE.test((e.text || '').trim()));
      } catch { /* dump 失败按空转计，下轮重试 */ }
      if (!hit) { idle += 1; await sleep(1000); continue; }
      idle = 0;
      run(['shell', 'input', 'tap', String(hit.center[0]), String(hit.center[1])]);
      grants.push(hit.text.trim());
      await sleep(1000); // 等下一枚弹窗渲染
    }
    const banner = await waitForBanner();
    return { cleared, banner, grants };
  };

  return {
    serial, adbPath, PACKAGE,
    install, launch, forceStop, clearLogcat, wake, setStayOn,
    factoryReset,
    dump, dumpSummary, find, findAll, tap, tapXY, inputText, swipe,
    screenshot, logcat, waitForBanner,
  };
}

// 直接 import 本模块时提供绑定 targets 配置的默认通道（惰性创建）
export default function androidChannel(opts) {
  return createAdbChannel(opts);
}
