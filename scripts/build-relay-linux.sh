#!/usr/bin/env bash
# 在 WSL Ubuntu-22.04 内构建 Linux x86_64 中继二进制(Linux 发布产物唯一构建路径,见 AGENTS.md)
# 用法(Windows Git Bash):  bash scripts/build-relay-linux.sh
# 产物: target-relay-linux/release/localtrans-relay(工作区内)
set -euo pipefail
cd "$(dirname "$0")/.."

# 构建产物放 WSL 原生文件系统,避开 /mnt/c 9p I/O 慢的问题;源码留在仓库原地
wsl -d Ubuntu-22.04 -- bash -lc '
set -euo pipefail
cd /mnt/c/Users/<user>/Desktop/work/localTrans
export CARGO_TARGET_DIR=~/relaytarget
cargo build --release -p localtrans-relay
cp -f ~/relaytarget/release/localtrans-relay /mnt/c/Users/<user>/Desktop/work/localTrans/target-relay-linux/
'
mkdir -p target-relay-linux
echo "产物: $(pwd)/target-relay-linux/localtrans-relay"
