#!/usr/bin/env bash
# 在 Linux 測試環境（需 root）建立 E2E 測試用的 sshd + ssh-agent。
#   - sshd 監聽 127.0.0.1:2201（標準設定）與 127.0.0.1:2222（模擬非標準 port）
#   - 測試帳號：builder、pi（登入 shell 為 bash）、dashy（登入 shell 為 dash）
#   - 產生 ed25519 + rsa 金鑰並載入 ssh-agent
#   - 產生 known_hosts 與 ssh-mcp 設定檔
# 用法：source tests/e2e/setup_linux.sh <work_dir>
set -euo pipefail
WORK="${1:?usage: setup_linux.sh <work_dir>}"
mkdir -p "$WORK"/{keys,hostkeys,logs,outputs}

# ---- 測試帳號 ----
for u in builder pi dashy; do
  id "$u" >/dev/null 2>&1 || useradd -m -s /bin/bash "$u"
done
usermod -s /bin/dash dashy
# 無密碼帳號預設是 locked，UsePAM no 時 sshd 會拒絕；設成 "*"（不可用密碼登入但未鎖定）
for u in builder pi dashy; do usermod -p "*" "$u"; done
for u in builder pi dashy; do
  h=$(getent passwd "$u" | cut -d: -f6)
  mkdir -p "$h/.ssh" && chmod 700 "$h/.ssh"
done

# ---- 用戶端金鑰 ----
[ -f "$WORK/keys/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -f "$WORK/keys/id_ed25519" -C e2e-ed25519
[ -f "$WORK/keys/id_rsa" ]     || ssh-keygen -q -t rsa -b 3072 -N '' -f "$WORK/keys/id_rsa" -C e2e-rsa
for u in builder pi dashy; do
  h=$(getent passwd "$u" | cut -d: -f6)
  cat "$WORK/keys/id_ed25519.pub" > "$h/.ssh/authorized_keys"
  chmod 600 "$h/.ssh/authorized_keys"; chown -R "$u:$u" "$h/.ssh"
done

# ---- sshd ----
[ -f "$WORK/hostkeys/ssh_host_ed25519_key" ] || ssh-keygen -q -t ed25519 -N '' -f "$WORK/hostkeys/ssh_host_ed25519_key"
mkdir -p /run/sshd
cat > "$WORK/sshd_config" <<CFG
ListenAddress 127.0.0.1:2201
ListenAddress 127.0.0.1:2222
HostKey $WORK/hostkeys/ssh_host_ed25519_key
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
UsePAM no
PidFile $WORK/sshd.pid
LogLevel VERBOSE
CFG
if [ -f "$WORK/sshd.pid" ] && kill -0 "$(cat "$WORK/sshd.pid")" 2>/dev/null; then
  kill "$(cat "$WORK/sshd.pid")"; sleep 0.5
fi
/usr/sbin/sshd -f "$WORK/sshd_config" -E "$WORK/logs/sshd.log"
sleep 0.5

# ---- ssh-agent：先放 rsa（不被接受），再放 ed25519，驗證逐把嘗試 ----
if [ -n "${SSH_AGENT_PID:-}" ]; then kill "$SSH_AGENT_PID" 2>/dev/null || true; fi
eval "$(ssh-agent -s -a "$WORK/agent.sock" 2>/dev/null || { rm -f "$WORK/agent.sock"; ssh-agent -s -a "$WORK/agent.sock"; })" >/dev/null
ssh-add -q "$WORK/keys/id_rsa" "$WORK/keys/id_ed25519"
echo "export SSH_AUTH_SOCK=$SSH_AUTH_SOCK SSH_AGENT_PID=$SSH_AGENT_PID" > "$WORK/agent.env"

# ---- known_hosts（2201 與 [127.0.0.1]:2222 兩種格式）----
ssh-keyscan -p 2201 127.0.0.1 2>/dev/null >  "$WORK/known_hosts"
ssh-keyscan -p 2222 127.0.0.1 2>/dev/null >> "$WORK/known_hosts"

# ---- ssh-mcp 設定檔 ----
cat > "$WORK/config.toml" <<CFG
[general]
default_timeout_s  = 10
max_timeout_s      = 60
max_output_bytes   = 4096
known_hosts        = "$WORK/known_hosts"
agent              = "env"
log_dir            = "$WORK/logs"
output_dir         = "$WORK/outputs"

[defaults]
user = "builder"
port = 2201
[defaults.env]
LANG = "C.UTF-8"

[[hosts]]
alias       = "build"
description = "builder@2201，含 cwd / env / pre_command"
host        = "127.0.0.1"
cwd         = "/home/builder"
pre_command = "source /home/builder/env.sh"
[hosts.env]
BB_NUMBER_THREADS = "8"
QUOTED = "it's \$HOME"

[[hosts]]
alias = "pi-2222"
description = "pi 帳號，非標準 port"
host  = "127.0.0.1"
port  = 2222
user  = "pi"

[[hosts]]
alias = "builder-2222"
description = "同主機不同帳號"
host  = "127.0.0.1"
port  = 2222

[[hosts]]
alias = "badpre"
host  = "127.0.0.1"
pre_command = "false"

[[hosts]]
alias = "raw"
host  = "127.0.0.1"
shell_mode = "raw"
cwd = "/tmp"

[[hosts]]
alias = "dash"
host  = "127.0.0.1"
user  = "dashy"
pre_command = ". /home/dashy/env.sh"

[[hosts]]
alias = "unknown-key"
host  = "localhost"
port  = 2201

[[hosts]]
alias = "danger"
host  = "127.0.0.1"
allow_dangerous = true
CFG

# pre_command 會 cd 到別的目錄（模擬 oe-init-build-env）
cat > /home/builder/env.sh <<'SH'
export FROM_PRE=yes
cd /
SH
echo 'export FROM_PRE=dash' > /home/dashy/env.sh
chown builder:builder /home/builder/env.sh; chown dashy:dashy /home/dashy/env.sh
echo "setup OK: $WORK"
