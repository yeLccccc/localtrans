#!/usr/bin/env bash
# LocalTrans 并行开发 worktree 管理脚本（Git Bash / Windows）
#
# 用法:
#   bash scripts/wt.sh new <任务名> <泳道>   # 泳道: core|relay|shell|ui|infra|read
#   bash scripts/wt.sh rm  <任务名>
#   bash scripts/wt.sh ls
#
# 约定:
#   - worktree 放在 D 盘（可用 LOCALTRANS_WT_BASE 覆盖），分支名 agent/<任务名>
#   - 每个 worktree 分配独立测试端口段: LOCALTRANS_TEST_PORT_BASE = 50000 + 槽位*100
#     (P0-1 落地后测试代码读取该变量; 落地前仅作预留约定)
set -euo pipefail

WT_BASE="${LOCALTRANS_WT_BASE:-D:/localTrans-wt}"
SLOT_FILE="$WT_BASE/.slots"

die() { echo "[wt] 错误: $*" >&2; exit 1; }

ROOT="$(git rev-parse --show-toplevel)"
mkdir -p "$WT_BASE"
touch "$SLOT_FILE"

slot_of() { # 任务名 -> 槽位号
    grep -n "^$1 " "$SLOT_FILE" | head -1 | cut -d: -f1
}

case "${1:-}" in
new)
    name="${2:?用法: wt.sh new <任务名> <泳道>}"
    lane="${3:?缺少泳道: core|relay|shell|ui|infra|read}"
    wt_dir="$WT_BASE/$name"
    [ -e "$wt_dir" ] && die "worktree 已存在: $wt_dir"

    # 分配最小空闲槽位（同时限制本机并行写任务上限的提醒线: 6）
    slot=0
    while grep -q " $slot$" "$SLOT_FILE"; do slot=$((slot + 1)); done
    port_base=$((50000 + slot * 100))

    echo "[wt] 创建 worktree: $wt_dir (分支 agent/$name, 泳道 $lane, 端口段 ${port_base}-$((port_base + 99)))"
    git -C "$ROOT" worktree add "$wt_dir" -b "agent/$name"
    echo "$name $lane $slot" >>"$SLOT_FILE"

    # 拷贝不入库的本机配置（Android 构建/签名需要）
    for f in android/local.properties android/keystore.properties android/keystore; do
        if [ -e "$ROOT/$f" ] && [ ! -e "$wt_dir/$f" ]; then
            mkdir -p "$(dirname "$wt_dir/$f")"
            cp -r "$ROOT/$f" "$wt_dir/$f"
            echo "[wt] 已拷贝本机配置: $f"
        fi
    done

    cat <<EOF
[wt] 就绪。启动 agent:
  cd $wt_dir
  # 环境变量(建议写入 agent 提示词): LOCALTRANS_TEST_PORT_BASE=$port_base
  # 提示: ui/node_modules 不会自动就位, 涉及前端先 cd ui && npm install
EOF
    ;;
rm)
    name="${2:?用法: wt.sh rm <任务名>}"
    wt_dir="$WT_BASE/$name"
    [ -e "$wt_dir" ] || die "worktree 不存在: $wt_dir"
    git -C "$ROOT" worktree remove --force "$wt_dir"
    git -C "$ROOT" branch -D "agent/$name" 2>/dev/null || true
    sed -i "/^$name /d" "$SLOT_FILE"
    echo "[wt] 已移除: $name"
    ;;
ls)
    echo "任务名            泳道    槽位  端口段"
    while read -r n l s; do
        printf "%-16s %-7s %-5s %d-%d\n" "$n" "$l" "$s" "$((50000 + s * 100))" "$((50000 + s * 100 + 99))"
    done <"$SLOT_FILE"
    [ -s "$SLOT_FILE" ] || echo "(空)"
    git -C "$ROOT" worktree list
    ;;
*)
    die "用法: wt.sh new|rm|ls ..."
    ;;
esac
