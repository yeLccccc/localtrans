#!/usr/bin/env bash
# Task M0: 发布产物"干净度"扫描（spec §7.1.1 发布期门，第三重门）。
#
# 用法: bash scripts/check-release-clean.sh <二进制路径>
#
# 在二进制字节流中搜索 test-api 特征串 "X-LocalTrans-TestAPI"
# （test-api 服务的全局响应头字面量，见 src-tauri/src/test_api/mod.rs）。
# 命中说明测试面混进了正式产物 → 退出 1；干净 → 退出 0；用法/文件错误 → 退出 2。
# 兼容 Git Bash / MSYS：Windows 风格路径经 cygpath 转换后供 grep 使用。

set -uo pipefail

MARKER="X-LocalTrans-TestAPI"

BIN="${1:-}"
if [[ -z "$BIN" ]]; then
    echo "用法: bash $0 <二进制路径>" >&2
    exit 2
fi

# Git Bash / MSYS 兼容：C:\...\x.exe → /c/.../x.exe（非 MSYS 环境原样使用）
if command -v cygpath >/dev/null 2>&1; then
    BIN_SCAN="$(cygpath -u "$BIN" 2>/dev/null || printf '%s' "$BIN")"
else
    BIN_SCAN="$BIN"
fi

if [[ ! -f "$BIN_SCAN" ]]; then
    echo "[check-release-clean] 错误: 文件不存在: $BIN" >&2
    exit 2
fi

# LANG=C + grep -a 强制按字节在二进制中搜索（-c 统计命中行数；
# 无命中时 grep 退出码为 1，用 || true 吞掉避免 set -e 类误判）
COUNT="$(LANG=C grep -a -c "$MARKER" "$BIN_SCAN" || true)"

if [[ "$COUNT" -gt 0 ]]; then
    echo "[check-release-clean] 违规: $BIN 中发现 test-api 特征串 $MARKER（命中 $COUNT 处）" >&2
    echo "[check-release-clean] 正式发布产物不得包含测试面（spec §7.1.1 发布期门），构建失败" >&2
    exit 1
fi

echo "[check-release-clean] 通过: $BIN 未包含 test-api 特征串 $MARKER"
exit 0
