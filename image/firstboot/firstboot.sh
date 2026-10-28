#!/bin/bash
# /usr/local/lib/ye-os/firstboot.sh
# 只在首次启动跑一次，由 ye-os-firstboot.service 调用。
# 必须幂等：后面的章节会删掉 firstboot.done 让它重跑。
set -euo pipefail

MARK=/var/lib/ye-os/firstboot.done
mkdir -p /var/lib/ye-os

if getent group ai-agent >/dev/null; then
  usermod -aG ai-agent ye
fi

touch "$MARK"