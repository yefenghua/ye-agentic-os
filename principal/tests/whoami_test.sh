#!/usr/bin/env bash
# 以 ai 身份证明三件事：写不了 /etc、没有能力、被拒的请求进了审计日志。
set -uo pipefail
RC=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; RC=1; }
[ "$(id -u)" -eq 0 ] || { echo "需要 root"; exit 2; }

# 和加固 drop-in 一样的处境：ai 用户、ai.slice、空能力集、不可提权。
as_ai() {
  systemd-run --quiet --wait --pipe --collect \
    --uid=ai --gid=ai-agent --slice=ai.slice \
    -p NoNewPrivileges=yes -p CapabilityBoundingSet= \
    "$@"
}

# 1. 身份与位置
out="$(as_ai id -un 2>&1)"
[ "$out" = "ai" ] && pass "以 ai 运行" || fail "id -un 得到: $out"
out="$(as_ai cat /proc/self/cgroup 2>&1)"
case "$out" in */ai.slice/*) pass "进程在 ai.slice 里";; *) fail "cgroup: $out";; esac

# 2. 直接写 /etc 必须失败
if as_ai sh -c 'echo x > /etc/ye-whoami-test' 2>/dev/null; then
  fail "ai 直接写入了 /etc"; rm -f /etc/ye-whoami-test
else
  pass "ai 不能直接写 /etc"
fi

# 3. 能力为空、不可提权
status="$(as_ai grep -E '^(CapBnd|CapEff|NoNewPrivs):' /proc/self/status 2>&1)"
echo "$status" | grep -q 'CapBnd:[[:space:]]*0000000000000000' && pass "能力边界集为空" || fail "CapBnd: $status"
echo "$status" | grep -q 'CapEff:[[:space:]]*0000000000000000' && pass "有效能力为空" || fail "CapEff: $status"
echo "$status" | grep -q 'NoNewPrivs:[[:space:]]*1' && pass "no_new_privs 已设" || fail "NoNewPrivs: $status"
# sudo 是 setuid 程序，在 no_new_privs 下不能提权
if as_ai sudo -n true 2>/dev/null; then fail "ai 竟能 sudo"; else pass "sudo 对 ai 无效"; fi

# 4. 代理拒绝 file.write_etc，并留下审计
before="$(journalctl --namespace=ai-audit -o cat --no-pager 2>/dev/null | grep -c '"decision":"deny"')"
out="$(as_ai busctl call --timeout=30 org.yeos.Broker1 /org/yeos/Broker1 org.yeos.Broker1 \
        Perform 'sa{sv}' file.write_etc 2 path s /etc/ye-whoami-test content s hello 2>&1)"
case "$out" in 'bs false "not authorized"') pass "代理拒绝 ai 写 /etc";; *) fail "代理返回: $out";; esac
[ -e /etc/ye-whoami-test ] && { fail "/etc/ye-whoami-test 竟然存在"; rm -f /etc/ye-whoami-test; }
sleep 0.5
after="$(journalctl --namespace=ai-audit -o cat --no-pager 2>/dev/null | grep -c '"decision":"deny"')"
[ "$after" -gt "$before" ] && pass "拒绝记录进了 ai-audit（$before -> $after）" || fail "审计里没有新的 deny"

# 5. 代理放行 ai 做它被允许的事（yetxn 装好之前返回明确的错误，但 decision 是 allow）
out="$(as_ai busctl call --timeout=30 org.yeos.Broker1 /org/yeos/Broker1 org.yeos.Broker1 \
        Perform 'sa{sv}' snapshot.create 1 actor s whoami 2>&1)"
last="$(journalctl --namespace=ai-audit -o cat --no-pager | grep '"action":"snapshot.create"' | tail -1)"
case "$last" in *'"decision":"allow"'*) pass "snapshot.create 对 ai 放行（$out）";; *) fail "snapshot.create 审计: $last";; esac

exit $RC