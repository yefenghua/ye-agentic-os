#!/bin/bash
# image/run.sh：在 macOS 上运行。
#   ./image/run.sh                 启动虚拟机
#   ./image/run.sh snapshot clean  做名为 clean 的快照
#   ./image/run.sh restore clean   恢复到 clean
set -euo pipefail
source "$(dirname "$0")/lib.sh"

case "${1:-run}" in
  run)      run_vm ;;
  snapshot) snapshot_vm "${2:-}" ;;
  restore)  restore_vm "${2:-}" ;;
  *)        die "用法: $0 [run|snapshot <名字>|restore <名字>]" ;;
esac