#!/bin/bash
# image/lib.sh：ye-os 镜像构建与运行的所有函数。
# 被 build.sh（Linux，root）和 run.sh（macOS）source。
# 不要用 bash 4 才有的语法，macOS 自带的是 bash 3.2。

set -euo pipefail

# 仓库根目录：lib.sh 在 image/ 下，往上一级。
YE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE_DIR="$YE_ROOT/image"
OUT_DIR="$IMAGE_DIR/out"

# 构建机本地的工作目录（见第 0.4 节）。
WORK="${YE_WORK:-/var/tmp/ye-os-build}"
MNT="$WORK/mnt"              # 镜像挂载点
ROOTFS_TAR="$WORK/rootfs.tar"
BUILD_IMG="$WORK/ye-os.img"  # 构建中的镜像
IMG="$OUT_DIR/ye-os.img"     # 导出后的镜像，run_vm 用这个

IMG_SIZE="${YE_IMG_SIZE:-20G}"
ESP_SIZE="${YE_ESP_SIZE:-512MiB}"

SUITE="${YE_SUITE:-testing}"          # Debian testing；想要更稳可以设 YE_SUITE=trixie
ARCH="arm64"
MIRROR="${YE_MIRROR:-https://deb.debian.org/debian}"
HOSTNAME_="ye-os"
TZ_="${YE_TZ:-Asia/Shanghai}"
DEFAULT_USER="ye"
DEFAULT_PASSWORD="ye"

# 基础包。内核不在这里，放在 install_kernel 里单独装。
BASE_PACKAGES="systemd,systemd-sysv,systemd-boot,systemd-resolved,systemd-timesyncd"
BASE_PACKAGES="$BASE_PACKAGES,udev,dbus,dbus-user-session,kmod,sudo,openssh-server"
BASE_PACKAGES="$BASE_PACKAGES,btrfs-progs,dosfstools,initramfs-tools,zstd"
BASE_PACKAGES="$BASE_PACKAGES,locales,tzdata,ca-certificates,curl,iproute2,netbase"
BASE_PACKAGES="$BASE_PACKAGES,less,vim-tiny,bash-completion,polkitd,python3"
BASE_PACKAGES="$BASE_PACKAGES,man-db,procps,psmisc,file,rsync,acl,git"
BASE_PACKAGES="$BASE_PACKAGES,build-essential,pkg-config,libssl-dev,cmake"
# 第 5、6 章的 Python+GTK 示例与提示程序，以及 wrap-cli 用的 col（bsdextrautils）。
BASE_PACKAGES="$BASE_PACKAGES,python3-gi,gir1.2-gtk-4.0,bsdextrautils"
BASE_PACKAGES="$BASE_PACKAGES,udev,dbus,dbus-user-session,kmod,sudo,passwd,openssh-server"
BASE_PACKAGES="$BASE_PACKAGES,mount,login"

# 虚拟机参数（macOS 侧）。
VM_CPUS="${YE_VM_CPUS:-4}"
VM_MEM="${YE_VM_MEM:-4096}"
SSH_PORT="${YE_SSH_PORT:-2222}"

log() { printf '\033[1;34m[ye-os]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[ye-os] 错误:\033[0m %s\n' "$*" >&2; exit 1; }

make_rootfs() {
  require_root
  mkdir -p "$WORK"
  log "mmdebstrap $SUITE/$ARCH -> $ROOTFS_TAR"
  mmdebstrap \
    --mode=root \
    --arch="$ARCH" \
    --variant=apt \
    --components=main \
    --include="$BASE_PACKAGES" \
    --aptopt='Acquire::Languages "none"' \
    "$SUITE" "$ROOTFS_TAR" "$MIRROR"
  log "rootfs 大小: $(du -h "$ROOTFS_TAR" | cut -f1)"
}

attach_image() {
  require_root
  # 已经挂过就复用，让函数可以在新 shell 里重复调用。
  if [ -f "$WORK/loopdev" ] && losetup "$(cat "$WORK/loopdev")" >/dev/null 2>&1; then
    LOOP="$(cat "$WORK/loopdev")"
    log "复用 loop 设备 $LOOP"
    return 0
  fi
  LOOP="$(losetup --find --show --partscan "$BUILD_IMG")"
  echo "$LOOP" > "$WORK/loopdev"
  # --partscan 让内核扫描分区表并建立 ${LOOP}p1、${LOOP}p2，但设备节点是异步出现的。
  udevadm settle
  log "镜像挂到 $LOOP"
}

detach_image() {
  require_root
  [ -f "$WORK/loopdev" ] || return 0
  local dev
  dev="$(cat "$WORK/loopdev")"
  if losetup "$dev" >/dev/null 2>&1; then
    losetup -d "$dev"
    log "释放 $dev"
  fi
  rm -f "$WORK/loopdev"
  LOOP=""
}

make_disk() {
  require_root
  mkdir -p "$WORK"
  detach_image                      # 上一次构建残留的 loop 设备
  rm -f "$BUILD_IMG"
  log "创建 $IMG_SIZE 稀疏镜像 $BUILD_IMG"
  truncate -s "$IMG_SIZE" "$BUILD_IMG"

  # GPT：1 号分区 ESP，2 号分区 Linux ARM64 root（可发现分区 GUID）。
  sgdisk --zap-all "$BUILD_IMG" >/dev/null
  sgdisk \
    --new=1:0:+"$ESP_SIZE" --typecode=1:ef00 --change-name=1:YE-ESP \
    --new=2:0:0           --typecode=2:8305 --change-name=2:YE-ROOT \
    "$BUILD_IMG" >/dev/null

  attach_image
  log "格式化 ${LOOP}p1 (vfat) 和 ${LOOP}p2 (btrfs)"
  mkfs.vfat -F 32 -n YE-ESP "${LOOP}p1" >/dev/null
  mkfs.btrfs -f -L YE-ROOT "${LOOP}p2" >/dev/null
}

make_subvolumes() {
  require_root
  [ -n "${LOOP:-}" ] || attach_image
  local top="$WORK/top"
  mkdir -p "$top"
  mount "${LOOP}p2" "$top"
  local sv
  for sv in @ @home @var @snapshots; do
    if [ ! -d "$top/$sv" ]; then
      btrfs subvolume create "$top/$sv" >/dev/null
      log "子卷 $sv"
    fi
  done
  # 把 @ 设为默认子卷：内核命令行里不写 rootflags=subvol=，initramfs 挂载
  # 根分区时就会挂到 @。第 2 章的回滚只需要换默认子卷，不用改引导条目。
  local root_id
  root_id="$(btrfs subvolume list "$top" | awk '$NF=="@"{print $2}')"
  [ -n "$root_id" ] || die "找不到子卷 @ 的 id"
  btrfs subvolume set-default "$root_id" "$top"
  log "默认子卷 -> @ (id $root_id)"
  umount "$top"
}

mount_image() {
  require_root
  [ -n "${LOOP:-}" ] || attach_image
  mkdir -p "$MNT"
  if mountpoint -q "$MNT"; then
    log "$MNT 已挂载"
    return 0
  fi
  local opts="noatime,compress=zstd:1"
  mount -o "subvol=@,$opts" "${LOOP}p2" "$MNT"
  mkdir -p "$MNT/home" "$MNT/var" "$MNT/.snapshots" "$MNT/efi"
  mount -o "subvol=@home,$opts"      "${LOOP}p2" "$MNT/home"
  mount -o "subvol=@var,$opts"       "${LOOP}p2" "$MNT/var"
  mount -o "subvol=@snapshots,noatime" "${LOOP}p2" "$MNT/.snapshots"
  mount -o umask=0077 "${LOOP}p1" "$MNT/efi"
  log "镜像挂在 $MNT"
}

umount_image() {
  require_root
  mountpoint -q "$MNT" || return 0
  # -R 递归卸载 $MNT 下所有挂载，顺序是先子后父。
  umount -R "$MNT"
  log "已卸载 $MNT"
}

populate_root() {
  require_root
  mountpoint -q "$MNT" || die "先 mount_image"
  [ -f "$ROOTFS_TAR" ] || die "先 make_rootfs"
  log "解压 rootfs 到 $MNT"
  tar -C "$MNT" \
      --xattrs --xattrs-include='*' \
      --numeric-owner \
      -xpf "$ROOTFS_TAR"
}

chroot_run() {
  require_root
  mountpoint -q "$MNT" || die "先 mount_image"
  # chroot 里的程序需要 /proc /sys /dev；/run 用 tmpfs，
  # 并给 systemd-resolved 的 stub-resolv.conf 符号链接放一个真实文件，
  # 这样 apt 在 chroot 里能解析域名。
  mount -t proc proc "$MNT/proc"
  mount --rbind /sys "$MNT/sys";  mount --make-rslave "$MNT/sys"
  mount --rbind /dev "$MNT/dev";  mount --make-rslave "$MNT/dev"
  mount -t tmpfs tmpfs "$MNT/run"
  mkdir -p "$MNT/run/systemd/resolve"
  cp -L /etc/resolv.conf "$MNT/run/systemd/resolve/stub-resolv.conf"
  # 阻止包安装时启动服务。
  printf '#!/bin/sh\nexit 101\n' > "$MNT/usr/sbin/policy-rc.d"
  chmod +x "$MNT/usr/sbin/policy-rc.d"

  local rc=0
  chroot "$MNT" /usr/bin/env -i \
    HOME=/root TERM="${TERM:-xterm}" LANG=C.UTF-8 \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    DEBIAN_FRONTEND=noninteractive \
    "$@" || rc=$?

  rm -f "$MNT/usr/sbin/policy-rc.d"
  umount -R "$MNT/run" "$MNT/dev" "$MNT/sys" "$MNT/proc"
  return $rc
}

configure_base() {
  require_root
  mountpoint -q "$MNT" || die "先 mount_image"
  log "基础配置"

  echo "$HOSTNAME_" > "$MNT/etc/hostname"
  cat > "$MNT/etc/hosts" <<EOF
127.0.0.1 localhost
127.0.1.1 $HOSTNAME_
::1       localhost ip6-localhost ip6-loopback
EOF

  ln -sf "/usr/share/zoneinfo/$TZ_" "$MNT/etc/localtime"
  echo "$TZ_" > "$MNT/etc/timezone"

  cat > "$MNT/etc/locale.gen" <<EOF
en_US.UTF-8 UTF-8
zh_CN.UTF-8 UTF-8
EOF
  chroot_run locale-gen >/dev/null
  echo 'LANG=en_US.UTF-8' > "$MNT/etc/default/locale"

  # fstab：按 00a-interfaces.md 第 3 节。根子卷显式 subvol=@。
  cat > "$MNT/etc/fstab" <<EOF
LABEL=YE-ROOT  /            btrfs  subvol=@,noatime,compress=zstd:1          0 0
LABEL=YE-ROOT  /home        btrfs  subvol=@home,noatime,compress=zstd:1      0 0
LABEL=YE-ROOT  /var         btrfs  subvol=@var,noatime,compress=zstd:1       0 0
LABEL=YE-ROOT  /.snapshots  btrfs  subvol=@snapshots,noatime                 0 0
LABEL=YE-ESP   /efi         vfat   umask=0077                                0 2
EOF

  # 默认用户 ye，密码 ye，在 sudo 组；root 锁定。
  if ! chroot_run id -u "$DEFAULT_USER" >/dev/null 2>&1; then
    chroot_run useradd --create-home --shell /bin/bash --groups sudo "$DEFAULT_USER"
  fi
  echo "$DEFAULT_USER:$DEFAULT_PASSWORD" | chroot_run chpasswd
  chroot_run passwd --lock root >/dev/null

  # 空的 machine-id：首次启动时 systemd 会生成。
  : > "$MNT/etc/machine-id"
}

install_kernel() {
  require_root
  mountpoint -q "$MNT/efi" || die "先 mount_image"
  log "配置 kernel-install"

  mkdir -p "$MNT/etc/kernel"
  # 内核命令行：根分区按标签找，不写 rootflags=subvol=，让 initramfs 挂
  # 默认子卷（make_subvolumes 已把默认子卷设为 @，第 2 章回滚靠的就是这点）。
  # 最后一个 console= 成为 /dev/console，所以把串口放在最后。
  cat > "$MNT/etc/kernel/cmdline" <<EOF
root=LABEL=YE-ROOT rw console=tty0 console=ttyAMA0,115200
EOF
  cat > "$MNT/etc/kernel/install.conf" <<EOF
layout=bls
BOOT_ROOT=/efi
EOF
  echo "ye-os" > "$MNT/etc/kernel/entry-token"

  # initramfs-tools：没有 swap，别去找 resume 设备。
  mkdir -p "$MNT/etc/initramfs-tools/conf.d"
  echo "RESUME=none" > "$MNT/etc/initramfs-tools/conf.d/resume"

  log "安装内核"
  chroot_run apt-get update -qq
  chroot_run apt-get install -y -qq linux-image-arm64

  # 找到刚装的内核版本，显式跑一次 kernel-install。
  # Debian 的 postinst 钩子可能已经跑过，再跑一次是幂等的。
  local kver
  kver="$(ls "$MNT/usr/lib/modules" | sort -V | tail -n1)"
  [ -n "$kver" ] || die "没找到内核模块目录"
  log "kernel-install add $kver"
  chroot_run kernel-install add "$kver" "/boot/vmlinuz-$kver" "/boot/initrd.img-$kver"
}

require_root() {
  [ "$(id -u)" -eq 0 ] || die "这个函数要以 root 运行（在 Lima 里 sudo -i）"
}

install_bootloader() {
  require_root
  mountpoint -q "$MNT/efi" || die "先 mount_image"
  log "bootctl install"
  # --no-variables：chroot 里没有 EFI 变量可写。
  # 没有 EFI 变量时固件走可移动介质路径 EFI/BOOT/BOOTAA64.EFI，bootctl 会一并放好。
  chroot_run bootctl install --esp-path=/efi --no-variables --make-entry-directory=yes
  cat > "$MNT/efi/loader/loader.conf" <<EOF
default ye-os-*
timeout 3
console-mode max
editor no
EOF
}

first_boot_units() {
  require_root
  mountpoint -q "$MNT" || die "先 mount_image"
  log "首次启动单元与网络"

  # 有线网卡 DHCP。QEMU 的 virtio-net 在客机里叫 enp0s1 之类，en* 能匹配。
  mkdir -p "$MNT/etc/systemd/network"
  cat > "$MNT/etc/systemd/network/20-wired.network" <<EOF
[Match]
Name=en* eth*

[Network]
DHCP=yes
EOF

  # 首次启动脚本和单元。
  install -D -m 0755 "$IMAGE_DIR/firstboot/firstboot.sh" \
      "$MNT/usr/local/lib/ye-os/firstboot.sh"
  install -D -m 0644 "$IMAGE_DIR/firstboot/ye-os-firstboot.service" \
      "$MNT/etc/systemd/system/ye-os-firstboot.service"

  chroot_run systemctl enable \
      systemd-networkd systemd-resolved systemd-timesyncd \
      ssh ye-os-firstboot >/dev/null
}

export_image() {
  require_root
  umount_image
  detach_image
  mkdir -p "$OUT_DIR"
  log "导出 $BUILD_IMG -> $IMG"
  cp --sparse=always "$BUILD_IMG" "$IMG"
  log "导出完成: $(du -h "$IMG" | cut -f1) 实际占用，$(du -h --apparent-size "$IMG" | cut -f1) 表观大小"
}

snapshot_vm() {
  local name="${1:-}"
  [ -n "$name" ] || die "用法: snapshot_vm <名字>"
  [ -f "$IMG" ] || die "没有 $IMG"
  pgrep -f "file=$IMG" >/dev/null && die "虚拟机还在运行，先关掉再快照"
  local cp_flag="--reflink=auto"
  [ "$(uname -s)" = "Darwin" ] && cp_flag="-c"
  cp $cp_flag "$IMG" "$IMG.snap-$name"
  [ -f "$OUT_DIR/efivars.fd" ] && cp $cp_flag "$OUT_DIR/efivars.fd" "$OUT_DIR/efivars.fd.snap-$name"
  log "快照 $name 完成"
}

restore_vm() {
  local name="${1:-}"
  [ -n "$name" ] || die "用法: restore_vm <名字>"
  [ -f "$IMG.snap-$name" ] || die "没有快照 $name"
  pgrep -f "file=$IMG" >/dev/null && die "虚拟机还在运行，先关掉再恢复"
  local cp_flag="--reflink=auto"
  [ "$(uname -s)" = "Darwin" ] && cp_flag="-c"
  rm -f "$IMG"
  cp $cp_flag "$IMG.snap-$name" "$IMG"
  if [ -f "$OUT_DIR/efivars.fd.snap-$name" ]; then
    rm -f "$OUT_DIR/efivars.fd"
    cp $cp_flag "$OUT_DIR/efivars.fd.snap-$name" "$OUT_DIR/efivars.fd"
  fi
  log "已恢复到快照 $name"
}

run_vm() {
  [ "$(uname -s)" = "Darwin" ] || die "run_vm 在 macOS 上跑"
  [ -f "$IMG" ] || die "没有 ${IMG}，先在 Lima 里跑 build.sh"
  command -v qemu-system-aarch64 >/dev/null || die "brew install qemu"

  local qemu_share
  qemu_share="$(brew --prefix)/share/qemu"
  local code="$qemu_share/edk2-aarch64-code.fd"
  local vars="$OUT_DIR/efivars.fd"
  [ -f "$code" ] || die "找不到 $code"
  # UEFI 变量区要可写，每台虚拟机一份副本。
  if [ ! -f "$vars" ]; then
    cp "$qemu_share/edk2-arm-vars.fd" "$vars"
    log "初始化 $vars"
  fi

  log "启动 ye-os：串口在本终端，Ctrl-A X 退出；SSH 端口 $SSH_PORT"
  exec qemu-system-aarch64 \
    -machine virt,highmem=on \
    -accel hvf \
    -cpu host \
    -smp "$VM_CPUS" \
    -m "$VM_MEM" \
    -drive if=pflash,format=raw,readonly=on,file="$code" \
    -drive if=pflash,format=raw,file="$vars" \
    -drive if=virtio,format=raw,file="$IMG" \
    -device virtio-net-pci,netdev=n0 \
    -netdev "user,id=n0,hostfwd=tcp::${SSH_PORT}-:22" \
    -device virtio-rng-pci \
    -display none \
    -serial mon:stdio
}

install_principal() {
  require_root
  mountpoint -q "$MNT" || die "先 mount_image"
  local src="$YE_ROOT/principal" dst="$MNT/usr/local/src/ye-os/principal"
  mkdir -p "$dst"
  rsync -a --delete --exclude target "$src/" "$dst/"
  if [ -f "$src/out/ye-broker" ]; then
    # setup.sh 的 install_broker 在这个路径找二进制
    install -D -m 0755 "$src/out/ye-broker" "$dst/broker/target/release/ye-broker"
  else
    log "principal/out/ye-broker 不存在，镜像里不含代理二进制（首次启动后手动安装）"
  fi
  chroot_run bash /usr/local/src/ye-os/principal/setup.sh
}