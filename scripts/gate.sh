#!/usr/bin/env bash
# LocalTrans 泳道测试门禁（命令与 docs/build-and-test.md 保持一致）
#
# 用法: bash scripts/gate.sh <core|shell|relay|ui|all>
# agent 声明"完成"前必须通过对应泳道门禁; 合并 main 前集成者跑 all。
set -euo pipefail

lane="${1:?用法: gate.sh <core|shell|relay|ui|all>}"

run() { echo "==> $*"; "$@"; }

case "$lane" in
core)
    run cargo test -p localtrans-core
    ;;
shell)
    run cargo test -p localtrans
    ;;
relay)
    # 端口竞争, 必须串行; 同一时刻本机只允许一个 relay 门禁在跑
    run cargo test -p localtrans-relay -- --test-threads=1
    ;;
ui)
    run bash -c "cd ui && npm run build"
    ;;
all)
    for l in core shell relay ui; do
        echo "======== 门禁: $l ========"
        bash "$0" "$l"
    done
    echo "======== 全部门禁通过 ========"
    ;;
*)
    echo "未知泳道: $lane (可选: core|shell|relay|ui|all)" >&2
    exit 1
    ;;
esac
