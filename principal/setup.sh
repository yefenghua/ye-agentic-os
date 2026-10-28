#!/usr/bin/env bash
# principal/setup.sh —— 在虚拟机里以 root 运行。幂等：跑多少次结果一样。
set -euo pipefail

PRINCIPAL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FILES="$PRINCIPAL_DIR/files"

AI_USER=ai
AI_GROUP=ai-agent
AI_HOME=/var/lib/ai
HARDENING_SRC=/usr/local/lib/ye-os/hardening          # 加固片段
HARDENING_OUT=/etc/systemd/system/ye-hardening.conf   # 拼好的共享 drop-in
BROKER_BIN="$PRINCIPAL_DIR/broker/target/release/ye-broker"

CHANGED=0   # 有文件变化时置 1，最后决定要不要 daemon-reload

log() { printf '\033[1;34m[principal]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[principal] 错误:\033[0m %s\n' "$*" >&2; exit 1; }

require_root() { [ "$(id -u)" -eq 0 ] || die "需要 root（sudo bash setup.sh）"; }

# chroot 里没有运行中的 systemd，这时只能 enable 不能 start。
have_systemd() { [ -d /run/systemd/system ]; }

# install_file 源 目标 [权限]：内容相同就不动，不同才写，并记下有变化。
install_file() {
  local src="$1" dst="$2" mode="${3:-0644}"
  if [ -f "$dst" ] && cmp -s "$src" "$dst"; then
    return 0
  fi
  install -D -m "$mode" "$src" "$dst"
  CHANGED=1
  log "写入 $dst"
}

reload_systemd() {
  have_systemd || return 0
  [ "$CHANGED" -eq 1 ] || return 0
  systemctl daemon-reload
  log "daemon-reload"
}

main() {
  require_root
  if [ $# -gt 0 ]; then
    local f
    for f in "$@"; do "$f"; done
    reload_systemd
    return
  fi
  create_principal
  make_slice
  bounding_set
  seccomp_profile
  render_hardening
  audit_sink
  polkit_rules
  install_broker
  reload_systemd
  log "第 1 章全部就位"
}

create_principal() {
  require_root
  if ! getent group "$AI_GROUP" >/dev/null; then
    groupadd --system "$AI_GROUP"
    log "建组 $AI_GROUP"
  fi
  if ! getent passwd "$AI_USER" >/dev/null; then
    useradd --system \
            --gid "$AI_GROUP" \
            --home-dir "$AI_HOME" --no-create-home \
            --shell /usr/sbin/nologin \
            --comment "ye-os AI agent" \
            "$AI_USER"
    log "建用户 $AI_USER"
  fi
  install -d -m 0750 -o "$AI_USER" -g "$AI_GROUP" "$AI_HOME"

  # 默认用户进组。id -nG 输出空格分隔的组名，逐行比对避免前缀误判。
  if id ye >/dev/null 2>&1 && ! id -nG ye | tr ' ' '\n' | grep -qx "$AI_GROUP"; then
    usermod -aG "$AI_GROUP" ye
    log "ye 加入 $AI_GROUP（重新登录后生效）"
  fi
}

make_slice() {
  require_root
  install_file "$FILES/ai.slice" /etc/systemd/system/ai.slice
  if have_systemd; then
    systemctl daemon-reload
    systemctl start ai.slice
  fi
}

bounding_set() {
  require_root
  install_file "$FILES/hardening/10-caps.conf" "$HARDENING_SRC/10-caps.conf"
}

seccomp_profile() {
  require_root
  install_file "$FILES/hardening/20-seccomp.conf" "$HARDENING_SRC/20-seccomp.conf"
}

# 把片段按文件名顺序拼成一个 drop-in。systemd 只读 *.service 之类的单元文件和
# *.d 目录，/etc/systemd/system/ye-hardening.conf 本身会被忽略，正合适。
render_hardening() {
  require_root
  [ -d "$HARDENING_SRC" ] || die "先跑 bounding_set 和 seccomp_profile"
  local tmp
  tmp="$(mktemp)"
  {
    echo "# 由 principal/setup.sh render_hardening 生成，不要手改。"
    echo "# 改 $HARDENING_SRC/*.conf 然后重跑。"
    cat "$HARDENING_SRC"/*.conf
  } > "$tmp"
  install_file "$tmp" "$HARDENING_OUT"
  rm -f "$tmp"
}

# attach_hardening 单元名：给一个单元挂上共享加固。
# 符号链接名里的 50 决定顺序：单元想覆盖某一项，在同目录放 60-override.conf，
# 后面的文件覆盖前面的（布尔和单值设置），列表设置则累加。
attach_hardening() {
  require_root
  local unit="$1"
  case "$unit" in *.service) ;; *) die "attach_hardening 需要 xxx.service";; esac
  local d="/etc/systemd/system/$unit.d"
  mkdir -p "$d"
  if [ "$(readlink "$d/50-ye-hardening.conf" 2>/dev/null)" != "../ye-hardening.conf" ]; then
    ln -sfn ../ye-hardening.conf "$d/50-ye-hardening.conf"
    CHANGED=1
    log "$unit 挂上加固"
  fi
}

# write_policy_xml 输出文件路径：生成 polkit 的 .policy。
write_policy_xml() {
  local out="$1"
  local -A desc=(
    [pkg.install]="安装软件包"
    [pkg.remove]="卸载软件包"
    [unit.restart]="重启系统服务"
    [file.write_etc]="写入 /etc 下的文件"
    [snapshot.create]="开始一个事务（创建快照）"
    [snapshot.commit]="提交一个事务"
    [snapshot.rollback]="回滚一个事务"
  )
  {
    cat <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
<policyconfig>
  <vendor>ye-os</vendor>
  <vendor_url>https://github.com/yefenghua/ye-os</vendor_url>
XML
    local a
    for a in pkg.install pkg.remove unit.restart file.write_etc \
             snapshot.create snapshot.commit snapshot.rollback; do
      cat <<XML
  <action id="org.yeos.broker.$a">
    <description>${desc[$a]}</description>
    <message>ye-os 代理请求：${desc[$a]}</message>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin</allow_active>
    </defaults>
  </action>
XML
    done
    echo "</policyconfig>"
  } > "$out"
}

polkit_rules() {
  require_root
  command -v pkaction >/dev/null || die "缺 polkitd，第 0 章镜像应已包含"
  local tmp
  tmp="$(mktemp)"
  write_policy_xml "$tmp"
  install_file "$tmp" /usr/share/polkit-1/actions/org.yeos.broker.policy
  rm -f "$tmp"
  install_file "$FILES/50-ye-broker.rules" /etc/polkit-1/rules.d/50-ye-broker.rules
  # polkitd 用 inotify 监视这两个目录，不需要重启它。
}

audit_sink() {
  require_root
  install_file "$FILES/journald@ai-audit.conf" /etc/systemd/journald@ai-audit.conf
  if have_systemd; then
    systemctl daemon-reload
    # 不用显式 enable：第一个带 LogNamespace=ai-audit 的单元启动时它会自动起来。
    # 这里先手动起一次，让 /var/log/journal/<machine-id>.ai-audit 立刻存在。
    systemctl start systemd-journald@ai-audit.service
  fi
}

install_broker() {
  require_root
  if [ -f "$BROKER_BIN" ]; then
    install_file "$BROKER_BIN" /usr/local/bin/ye-broker 0755
  elif [ ! -x /usr/local/bin/ye-broker ]; then
    log "没有 $BROKER_BIN，先在客机里 cargo build --release；跳过二进制安装"
  fi
  install_file "$FILES/org.yeos.Broker1.conf" /etc/dbus-1/system.d/org.yeos.Broker1.conf
  install_file "$FILES/ye-broker.service" /etc/systemd/system/ye-broker.service

  if have_systemd; then
    # 让 dbus-daemon 重读策略目录，不用重启它。
    busctl call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig >/dev/null
    systemctl daemon-reload
    if [ -x /usr/local/bin/ye-broker ]; then
      systemctl enable --now ye-broker.service
      # 二进制换了就重启，拿到新代码。
      [ "$CHANGED" -eq 1 ] && systemctl restart ye-broker.service
      log "ye-broker $(systemctl is-active ye-broker.service)"
    fi
  else
    systemctl enable ye-broker.service   # chroot 里只建符号链接
  fi
}

# 被 source 时不执行，方便逐个函数验证。
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi