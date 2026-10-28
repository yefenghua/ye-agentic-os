#!/bin/bash
# image/build.sh：在 Lima 构建机里以 root 运行。
#   sudo /Users/yefenghua/Codes/ye-os/image/build.sh
set -euo pipefail
source "$(dirname "$(readlink -f "$0")")/lib.sh"
require_root

cleanup() {
  umount_image 2>/dev/null || true
  detach_image 2>/dev/null || true
}
trap cleanup EXIT

make_rootfs
make_disk
make_subvolumes
mount_image
populate_root
configure_base
install_kernel
install_bootloader
first_boot_units
install_principal
export_image

log "完成。回到 macOS 执行 image/run.sh"