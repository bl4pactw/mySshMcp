# mySshMcp 計劃書

> SSH MCP Server — Rust + rmcp + russh，STDIO transport，Windows x86_64 單一執行檔
> 版本：v0.1 規劃 ｜ 日期：2026-10-30 ｜ 作者：Brad

---

## 1. 目標與範圍

### 1.1 目標

- 讓 MCP client（Claude Desktop / Claude Code 等）透過 SSH 對指定主機**執行命令並取回結果**。
- 發佈形式為**單一 `ssh-mcp.exe`**：使用者不需安裝 Python、uv、pip、VC++ Redistributable 或任何套件。
- 僅限內部 / 少數人使用，不公開發佈。

### 1.2 非目標（v0.1 不做）

| 項目 | 原因 |
|---|---|
| 互動式 shell / PTY（vim、top） | LLM 不適合操作互動介面，且狀態難以控管 |
| 持續性 shell session（保持同一個 shell 不斷送命令） | **確定不做**。判斷輸出結束、timeout 後狀態不明、互動命令卡住等問題難以穩定處理。改用「無狀態執行 + 自動記住 cwd + 主機初始化設定」（§4.2、§5、§8.2） |
| HTTP / SSE transport | 不開 port，維持最小攻擊面 |
| 密碼認證、儲存私鑰 | 一律交給 ssh-agent，exe 不經手任何秘密 |
| Port forwarding / Jump host | v0.2 以後視需求評估 |
| 多平台（Linux/macOS） | 程式碼盡量保持可攜，但只驗證 Windows |

---

## 2. 架構

```text
┌───────────────┐  stdin/stdout   ┌──────────────────────────┐ SSH (TCP, port   ┌──────────┐
│  MCP Client   │ ◄─ JSON-RPC ──► │      ssh-mcp.exe         │ ◄──────────────► │  sshd    │
│ (Claude 等)   │                 │                          │ 依主機設定，預設22)│ (target) │
└───────────────┘                 │  rmcp server             │                  └──────────┘
                                  │   ├─ tools (exec/list/…) │
                                  │   ├─ guard (政策檢查)     │
                                  │   ├─ session pool        │
                                  │   ├─ audit log           │
                                  │   └─ config loader       │
                                  │  russh client            │
                                  └──────────┬───────────────┘
                                             │ named pipe
                                  ┌──────────▼───────────────┐
                                  │ Windows OpenSSH Agent    │
                                  │ \\.\pipe\openssh-ssh-agent│
                                  └──────────────────────────┘
```

- **STDIO**：由 MCP client 啟動子行程，不監聽任何網路 port。
- **stdout 只能輸出 JSON-RPC**。所有 log 走 stderr 或檔案（`tracing` 設定成只寫 stderr 和檔案）。
- **認證**：只透過 ssh-agent 簽章，exe 不讀取私鑰檔。
- **連線參數**：主機位址、port、帳號全部來自設定檔（§5），程式中不寫死任何預設以外的值；port 未設定時才使用 22。

---

## 3. 技術選型

| 元件 | 選擇 | 說明 |
|---|---|---|
| 語言 / 工具鏈 | Rust stable，`x86_64-pc-windows-msvc` | 靜態 CRT，產出單一 exe |
| MCP SDK | `rmcp`（官方 Rust SDK），features：`server`、`transport-io`、`macros` | 用 `#[tool_router]` / `#[tool]` 巨集定義工具 |
| SSH | `russh`（含 `russh::keys`） | 純 Rust，Windows 不需 OpenSSL |
| 加密後端 | **`ring`**（關閉 russh 預設的 `aws-lc-rs`） | Windows 上只需要 MSVC 編譯器；`aws-lc-rs` 另需 CMake + NASM |
| Async runtime | `tokio`（`rt-multi-thread`、`macros`、`time`、`io-std`） | rmcp / russh 都基於 tokio |
| 設定 | `serde` + `toml` | |
| JSON Schema | `schemars` | rmcp 用來產生工具參數 schema |
| Log | `tracing` + `tracing-subscriber` + `tracing-appender` | 寫 stderr + 每日輪替檔案 |
| 錯誤處理 | `anyhow`（內部）、`thiserror`（對外錯誤型別） | |
| 路徑 | `directories` | 取得 `%APPDATA%` |

> **Phase 0 結果（2026-10-30）**：鎖定 `rmcp = "=3.8.0"`、`russh = "=0.65.0"`，並提交 `Cargo.lock`。russh 0.65 內建 Windows OpenSSH Agent（`AgentClient::connect_named_pipe`）與 Pageant（`connect_pageant`）支援，以及 `check_known_hosts_path`（支援 hashed hostname 與 `[host]:port`）。

---

## 4. 工具介面規格

原則：**工具越少越好**，參數用 host alias，不讓 LLM 傳任意 IP / 帳號。

### 4.1 `ssh_list_hosts`（唯讀）

列出設定檔中可用的主機。

```jsonc
// 輸入：無
// 輸出
{
  "hosts": [
    { "alias": "build-01", "description": "Yocto build server", "user": "builder", "host": "build-01.lan", "port": 22, "cwd": "/home/builder/poky/build", "has_pre_command": true, "allow_dangerous": false },
    { "alias": "lab-rpi", "description": "Lab Raspberry Pi", "user": "pi", "host": "lab-rpi.lan", "port": 2222, "cwd": null, "has_pre_command": false, "allow_dangerous": false }
  ]
}
```

### 4.2 `ssh_exec`（⚠ destructiveHint）

執行模式：**無狀態執行 + 自動記住 cwd + 主機初始化設定**。

- 每次呼叫都開新 channel、啟動新的遠端 shell，執行完就結束。
- SSH 連線本身由連線池保留重用（§8.3），不用每次重新認證。
- server 替每個 host alias 記住一個「目前工作目錄」（cwd）。每次執行都先 `cd` 到這個目錄，命令結束後再用 `pwd` 回報新的目錄並更新。所以 LLM 下 `cd src` 之後，下一個命令就會在 `src` 執行，行為類似一般終端機。
- **環境變數不會跨呼叫保留**。需要固定環境（例如 source SDK 環境）時，用設定檔的 `env` / `pre_command`（§5），每次執行都自動套用。

```jsonc
// 輸入
{
  "host": "build-01",          // 必填，必須是 config 中的 alias
  "command": "make -j8",       // 必填，可多行
  "timeout_s": 30,             // 選填，預設 30，上限由 config 限制（例如 600）
  "cwd": "/home/builder/src",  // 選填，這次改在指定目錄執行；成功後也會成為新的記住目錄
  "reset_cwd": false           // 選填，true = 先把記住的目錄重設為設定檔的 cwd（沒設定則為登入目錄）
}
// 輸出
{
  "exit_code": 0,              // 遠端沒回傳 exit-status 時為 null
  "exit_signal": null,         // 被 signal 終止時填入，例如 "KILL"
  "stdout": "...",             // 已移除 server 內部使用的 cwd 標記行
  "stderr": "...",
  "cwd": "/home/builder/src",  // 執行後記住的目錄
  "cwd_updated": true,         // false = 沒取得新目錄（例如命令中途 exit 或 timeout），沿用原本的目錄
  "truncated": false,          // 超過上限時為 true
  "full_output_path": null,    // 截斷時，完整輸出存到本地檔案的路徑
  "duration_ms": 123,
  "timed_out": false
}
```

`ssh_list_hosts` 的輸出也會加上每個 alias 目前記住的 `cwd`，讓 LLM 可以查詢目前位置。

### 4.3 `ssh_upload` / `ssh_download`（v0.2，走 SFTP）

- 使用 `russh-sftp`。
- 本地路徑限制在 config 指定的 `transfer_dir` 內，避免 LLM 讀寫任意本地檔案。
- `ssh_upload` 標記 destructiveHint。

### 4.4 Tool annotations

| 工具 | readOnlyHint | destructiveHint | openWorldHint |
|---|---|---|---|
| `ssh_list_hosts` | true | false | false |
| `ssh_exec` | false | true | true |
| `ssh_download` | false | false | true |
| `ssh_upload` | false | true | true |

---

## 5. 設定檔

位置（優先序）：

1. 環境變數 `SSH_MCP_CONFIG` 指定的路徑
2. `%APPDATA%\ssh-mcp\config.toml`

```toml
# %APPDATA%\ssh-mcp\config.toml

[general]
default_timeout_s   = 30
max_timeout_s       = 600
max_output_bytes    = 32768          # 回傳給 LLM 的上限（stdout+stderr 各自）
known_hosts         = "~/.ssh/known_hosts"
agent               = "openssh"      # "openssh" | "pageant"
log_dir             = "%APPDATA%\\ssh-mcp\\logs"
idle_session_ttl_s  = 300            # 連線池中閒置連線的保留時間

# 所有主機的預設連線參數；[[hosts]] 中有設定的欄位會覆蓋這裡
[defaults]
port               = 22
user               = "builder"
connect_timeout_s  = 10
keepalive_s        = 30

[[hosts]]
alias       = "build-01"
description = "Yocto build server"
host        = "build-01.lan"         # 主機名稱或 IP
# port / user 未設定 → 使用 [defaults]（22 / builder）

# --- 主機初始化設定：每次 ssh_exec 都自動套用 ---
cwd         = "/home/builder/poky/build"     # 初始工作目錄；未設定 = 遠端登入目錄
pre_command = "source /home/builder/poky/oe-init-build-env /home/builder/poky/build > /dev/null"
                                              # 每次執行前先跑；失敗則不執行命令並回報錯誤
                                              # 注意：oe-init-build-env 會 cd 到 build 目錄，
                                              # 執行順序是先 cd 記住的 cwd → pre_command → 再 cd 回記住的 cwd（§8.2）

[hosts.env]                                   # 每次執行都 export；值一律以單引號跳脫
LANG       = "C.UTF-8"
BB_NUMBER_THREADS = "8"

[[hosts]]
alias           = "lab-rpi"
description     = "Lab Raspberry Pi"
host            = "192.168.10.50"
port            = 2222               # 非標準 port
user            = "pi"               # 此主機使用不同帳號
allow_dangerous = false              # true 才允許 guard 的 dangerous 規則放行

# 同一台主機、不同帳號 → 建立兩個 alias，權限分開管理
[[hosts]]
alias       = "lab-rpi-admin"
description = "Lab Raspberry Pi (admin 帳號，僅必要時使用)"
host        = "192.168.10.50"
port        = 2222
user        = "admin"

[guard]
# 只是第二道防線；主要防線是遠端帳號權限（見 §6）
deny_patterns = [
  '(^|[;&|\s])rm\s+-[a-zA-Z]*r[a-zA-Z]*f?\s+/(\s|$)',
  '\bmkfs(\.\w+)?\b',
  '\bdd\b.*\bof=/dev/',
  '\b(shutdown|poweroff|halt|reboot)\b',
  ':\(\)\s*\{\s*:\|:&\s*\};:',
]
```

### 5.1 連線參數規則

| 欄位 | 必填 | 來源優先序 | 驗證 |
|---|---|---|---|
| `alias` | 是 | `[[hosts]]` | 不可重複；只允許 `[A-Za-z0-9._-]` |
| `host` | 是 | `[[hosts]]` | 主機名稱或 IPv4/IPv6 |
| `port` | 否 | `[[hosts]]` → `[defaults]` → `22` | 1–65535 |
| `user` | 否 | `[[hosts]]` → `[defaults]` | 兩者都沒有時，該主機載入失敗並在 log 報錯 |
| `connect_timeout_s` / `keepalive_s` | 否 | `[[hosts]]` → `[defaults]` → 內建值（10 / 30） | > 0 |
| `cwd` | 否 | `[[hosts]]` → `[defaults]` → 遠端登入目錄 | 必須是絕對路徑（`/` 開頭） |
| `pre_command` | 否 | `[[hosts]]` → `[defaults]` | 單行或多行 shell；設定檔是可信來源，不經 guard 檢查 |
| `env` | 否 | `[defaults.env]` 與 `[hosts.env]` **合併**，同名以 host 為準 | key 只允許 `[A-Za-z_][A-Za-z0-9_]*` |
| `shell_mode` | 否 | `[[hosts]]` → `[defaults]` → `"posix"` | `"posix"`：使用 §8.2 的包裝（支援 cwd 記憶、env、pre_command）；`"raw"`：原樣送出命令，不支援這三項（給非 POSIX 登入 shell 的主機，例如 Windows OpenSSH Server） |

- LLM **只能指定 `alias`**，不能透過工具參數覆蓋 host、port 或 user。要改連線目標，只能改設定檔。
- 同一台主機需要不同帳號時，建立不同 alias。每個 alias 可以有各自的 `allow_dangerous`，例如一般帳號和 admin 帳號分開控管。
- 非 22 port 的主機金鑰在 `known_hosts` 中格式為 `[host]:port`，驗證時要以此格式比對（見 §8.4）。
- 設定檔變更後需要重新啟動 MCP server（v0.1 不做熱重載）。
- 記住的 cwd 只存在 server 記憶體中。server 重新啟動（例如 Claude Desktop 重開）後，會回到設定檔的 `cwd`。
- LLM 能改變的只有記住的 cwd；`env` 和 `pre_command` 只能透過設定檔修改。
- `--check-config` 會列出每個 alias 套用預設值後的最終 host / port / user，方便確認。

> 不支援在 config 中放密碼或私鑰路徑。這是刻意的設計。

---

## 6. 安全設計

主要風險**不是外部入侵，而是 LLM 自己執行破壞性命令**，例如遠端輸出夾帶 prompt injection。採多層防護：

| 層級 | 措施 | 誰負責 |
|---|---|---|
| L1 遠端權限 | 每台主機使用**專用低權限帳號**；`sudoers` 只開放必要命令。這是最可靠的防線 | 主機管理者 |
| L2 認證 | 只用 ssh-agent，exe 不經手私鑰；不支援密碼 | ssh-mcp |
| L3 主機驗證 | 嚴格比對 `known_hosts`；未知或不符的主機金鑰**一律拒絕**（不做 TOFU 自動加入） | ssh-mcp |
| L4 主機白名單 | 只能連 config 中的 alias | ssh-mcp |
| L5 Guard | `deny_patterns` 黑名單；主機設定 `allow_dangerous=true` **且**環境變數 `SSH_MCP_ALLOW_DANGEROUS=1` 才放行。LLM 無法自己開啟 | ssh-mcp |
| L6 使用者確認 | `ssh_exec` 標記 destructiveHint；client 端**不要設成自動核准** | 使用者 |
| L7 資源限制 | timeout、輸出上限、同時連線數上限 | ssh-mcp |
| L8 稽核 | 每次呼叫寫入 `audit-YYYYMMDD.jsonl` | ssh-mcp |

稽核記錄格式（與 myAdbMcp 一致）：

```json
{"ts":"2026-10-30T10:00:00+08:00","tool":"ssh_exec","host":"build-01","command":"uname -a","exit_code":0,"duration_ms":123,"blocked":false,"truncated":false}
```

**已知限制**：黑名單一定能被繞過（例如 base64、變數展開、`sh -c`），只能擋誤用，不能擋刻意攻擊，所以 L1 一定要做。

---

## 7. 專案目錄結構

```text
mySshMcp/
├── PLAN.md                     # 本文件
├── README.md                   # 使用者安裝 / 設定說明
├── Cargo.toml
├── Cargo.lock                  # 必須提交（鎖定依賴版本）
├── .cargo/
│   └── config.toml             # crt-static 等建置設定
├── config.example.toml
├── src/
│   ├── main.rs                 # 進入點：參數解析、log 初始化、啟動 stdio server
│   ├── config.rs               # 設定檔載入與驗證
│   ├── server.rs               # rmcp ServerHandler + tool router
│   ├── tools/
│   │   ├── mod.rs
│   │   ├── list_hosts.rs
│   │   └── exec.rs
│   ├── ssh/
│   │   ├── mod.rs
│   │   ├── client.rs           # russh Handler（含 known_hosts 驗證）
│   │   ├── agent.rs            # OpenSSH named pipe / Pageant
│   │   ├── pool.rs             # 連線池：重用連線、閒置回收、斷線重連
│   │   ├── exec.rs             # 開 channel、收 stdout/stderr/exit-status
│   │   ├── wrap.rs             # 產生 §8.2 包裝 script、單引號跳脫、解析 cwd / 錯誤標記
│   │   └── state.rs            # 每個 alias 記住的 cwd（記憶體）
│   ├── guard.rs                # deny_patterns 檢查
│   ├── audit.rs                # JSONL 稽核記錄
│   └── output.rs               # 截斷、超量時存檔
└── tests/
    ├── guard_test.rs
    ├── output_test.rs
    └── e2e/
        ├── setup_linux.sh      # 建立測試用 sshd（port 2201 / 2222）、帳號、ssh-agent、設定檔
        └── run_e2e.py          # 以 JSON-RPC 驅動 ssh-mcp 的驗收測試
```

---

## 8. 實作要點與陷阱

### 8.1 STDIO

- `main.rs` 一開始就設定 `tracing` 只輸出到 stderr 和檔案。
- 程式中禁止使用 `println!`。在 crate 開頭加上 `clippy::print_stdout` lint（`#![deny(clippy::print_stdout)]`）。
- panic hook 也要導向 stderr 或 log 檔。

### 8.2 SSH 執行

- 每次 `exec` 都開新 channel，遠端 shell 執行完就結束。shell 本身的狀態（`export`、shell 變數、背景工作）不會保留；**只有 cwd 由 server 記住並在下次重新 `cd`**。
- `shell_mode = "posix"` 時，server 把命令包成以下 script 再送出（`<nonce>` 為每次呼叫隨機產生，避免和命令輸出撞名）：

  ```sh
  # 1) 套用設定檔的 env（值以單引號跳脫）
  export LANG='C.UTF-8' BB_NUMBER_THREADS='8'
  # 2) 進入記住的 cwd；失敗則回報並結束
  cd -- '/home/builder/poky/build' || { printf '__SSHMCP_<nonce>_ERR=cwd\n' >&2; exit 254; }
  # 3) pre_command；失敗則回報並結束
  { source /home/builder/poky/oe-init-build-env /home/builder/poky/build > /dev/null; } \
    || { printf '__SSHMCP_<nonce>_ERR=pre_command\n' >&2; exit 253; }
  # 4) pre_command 可能自己 cd，所以再 cd 回記住的 cwd
  cd -- '/home/builder/poky/build'
  # 5) 使用者命令（獨立成行，避免命令結尾的 # 註解吃掉後面的程式）
  make -j8
  # 6) 保存 exit code，回報結束時的目錄
  __sshmcp_rc=$?
  printf '\n__SSHMCP_<nonce>_CWD=%s\n' "$(pwd)"
  exit $__sshmcp_rc
  ```

  - 回報 cwd 用 `pwd`，不去解析 `cd` 命令本身，所以 `cd ..`、`pushd`、script 裡的 cd 都能正確記住。
  - server 從 stdout **最後一個**符合 `__SSHMCP_<nonce>_CWD=` 的行取出新目錄，並把這行從回傳給 LLM 的 stdout 中移除。
  - **必須先解析完整輸出，再做截斷**（§8.6），否則標記行可能被截掉。
  - 使用者命令中有 `exit N`、被 signal 終止或 timeout 時，第 6 步不會執行：沿用原本記住的目錄，回傳 `cwd_updated: false`。
  - exit 254（cwd 不存在，例如目錄被刪掉）：回傳 `isError`，提示 LLM 用 `reset_cwd: true` 或指定 `cwd`。
  - exit 253（pre_command 失敗）：回傳 `isError`，並附上 pre_command 的 stderr，提示檢查設定檔。
  - 判斷 254/253 是否為 server 的錯誤時，要同時檢查 stderr 中帶 nonce 的 `__SSHMCP_<nonce>_ERR=` 標記，避免和使用者命令本身回傳的 254/253 混淆。
  - `source` 是 bash 語法；遠端登入 shell 若為 dash 等純 POSIX shell，pre_command 要改用 `.`。文件中提醒使用者。
- 同一個 alias 有多個 exec 同時執行時，記住的 cwd 以**最後完成**的那次為準，並在文件中說明。
- `shell_mode = "raw"`：命令原樣送出，不包裝；不支援 cwd 記憶、env、pre_command（輸出中 `cwd` 為 null）。
- 讀取 channel 訊息：`Data` → stdout，`ExtendedData(ext=1)` → stderr，`ExitStatus` → exit_code，`ExitSignal` → exit_signal，收到 `Eof` 加 `Close` 才結束。
- **注意**：`ExitStatus` 可能比最後一筆 `Data` 先到，要等到 channel close 才算完成。
- Timeout：用 `tokio::time::timeout` 包住整個讀取迴圈。逾時就關閉 channel、回傳 `timed_out: true`，並附上已收到的部分輸出。
  - 遠端行程不一定會被終止。可選擇在命令前加 `timeout <N>`（若遠端有 coreutils），或文件中提醒使用者。

### 8.3 連線池

- key = host alias；value = `Arc<Handle>` 加上最後使用時間。
- 使用前檢查連線是否還活著（`is_closed()`）；斷線就重連一次。
- 背景任務定期回收閒置超過 `idle_session_ttl_s` 的連線。
- 設定 keepalive（russh `Config` 的 `keepalive_interval`），避免被 NAT 或防火牆切斷。
- 每台主機同時開啟的 channel 數設上限（`Semaphore`），避免碰到 sshd 的 `MaxSessions`（預設 10）。

### 8.4 known_hosts

- 使用 `russh::keys::check_known_hosts_path`。
- 要處理的情況：hashed hostname（`|1|...`）、非 22 port 的 `[host]:port` 格式、主機金鑰不符時給出明確錯誤訊息（提示使用者先用 `ssh` 手動連一次）。

### 8.5 ssh-agent（Windows）

- OpenSSH Agent：named pipe `\\.\pipe\openssh-ssh-agent`。需要服務 `ssh-agent` 已啟動，且已 `ssh-add`。
- Pageant：`AgentClient::connect_pageant`（設定 `agent = "pageant"`）。
- agent 沒有金鑰時，錯誤訊息要明確，例如：「ssh-agent 中沒有可用金鑰，請執行 `ssh-add`」。

### 8.6 輸出處理

- stdout 和 stderr 各自限制 `max_output_bytes`，保留頭尾（例如前 60%、後 40%），中間插入 `... [truncated N bytes] ...`。
- 超過上限時，完整輸出存到 `%APPDATA%\ssh-mcp\outputs\<timestamp>-<host>.log`，並回傳路徑。
- 非 UTF-8 輸出用 `String::from_utf8_lossy`，避免整個呼叫失敗。

### 8.7 錯誤回報

- 連線、認證、主機金鑰、guard 擋下等錯誤，用 MCP tool result 的 `isError: true` 回傳可讀訊息，不要讓 server 崩潰或回傳 JSON-RPC 協定錯誤。

---

## 9. 建置與發佈

### 9.1 建置設定

```toml
# .cargo/config.toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

```toml
# Cargo.toml（節錄）
[profile.release]
opt-level     = 3
lto           = "thin"
codegen-units = 1
strip         = true
panic         = "abort"
```

### 9.2 本機建置（v0.1 不做 CI）

- 建置環境：Windows 上的 Rust（MSVC toolchain）+ Visual Studio Build Tools（使用 ring 後端，不需要 CMake / NASM）。
- 發佈前手動執行：`cargo fmt --check` → `cargo clippy -- -D warnings` → `cargo test` → `cargo build --release`。
- 產出 `target\release\ssh-mcp.exe`，並產生 SHA256（`certutil -hashfile ssh-mcp.exe SHA256`）一起提供。
- 驗證是否還依賴 VC++ runtime：`dumpbin /dependents ssh-mcp.exe`，不應出現 `VCRUNTIME140.dll`。
- CI 自動建置之後有需要再評估，不在本計劃範圍內。

### 9.3 發佈注意

- **SmartScreen**：沒有簽章的 exe 第一次執行會跳警告。內部使用可考慮 code signing，或請 IT 加入白名單。
- 提供 `ssh-mcp.exe --version` 和 `--check-config`（只驗證設定檔、測試 agent 連線，不啟動 server），方便使用者自行排查。

---

## 10. 測試計畫

| 類型 | 內容 | 工具 |
|---|---|---|
| 單元測試 | guard 規則、輸出截斷、設定檔驗證、單引號跳脫（含 `'`、換行、`$`）、包裝 script 產生、cwd 標記解析與移除、env/defaults 合併 | `cargo test` |
| 整合測試 | Linux / WSL 中的 `openssh-server`：exec 成功、非零 exit code、stderr、timeout、大量輸出、斷線重連、主機金鑰不符、未知主機 | `tests/e2e/setup_linux.sh` + `run_e2e.py` |
| MCP 協定 | 工具清單、schema、annotations、錯誤回傳格式 | MCP Inspector（`npx @modelcontextprotocol/inspector`） |
| 實機 | Claude Desktop 載入、實際連公司內部主機 | 手動 |

驗收案例：

1. `ssh_exec(host="build-01", command="echo hi; echo err >&2; exit 3")` 回傳 stdout=`hi`、stderr=`err`、exit_code=3。
2. `command="sleep 100"` 且 `timeout_s=2`：約 2 秒後回傳 `timed_out: true`。
3. `command="seq 1 1000000"`：`truncated: true`，並存有完整輸出檔案。
4. 主機不在 config 中：`isError`，提示可用的 alias。
5. 修改 known_hosts 中的金鑰：拒絕連線，並給出明確訊息。
6. `rm -rf /`：被 guard 擋下，稽核記錄中 `blocked: true`。
6a. 設定 `port = 2222` 的主機（測試用 sshd 監聽 2222）可正常連線，且 known_hosts 以 `[host]:2222` 比對成功；未設定 port 的主機使用 `[defaults]` 的值。
6b. 同一台主機的兩個 alias（不同 user）各自用正確帳號登入（以 `whoami` 驗證）。
6c. cwd 記憶：`cd /tmp` → 下一次 `pwd` 回傳 `/tmp`；`cd ..`、`cd -` 也正確；`reset_cwd: true` 回到設定檔的 `cwd`；server 重啟後回到設定檔的 `cwd`。
6d. `cd /tmp; exit 0`：`cwd_updated: false`，記住的目錄不變。
6e. 記住的目錄被刪除：回傳 `isError` 並提示 `reset_cwd`。
6f. env / pre_command：`echo $BB_NUMBER_THREADS` 回傳 `8`；pre_command 故意失敗時回傳 `isError`，且使用者命令沒有被執行。
6g. pre_command 會 cd 到別的目錄（模擬 `oe-init-build-env`）：使用者命令仍在記住的 cwd 執行。
6h. 回傳給 LLM 的 stdout 中不含任何 `__SSHMCP_` 標記；大量輸出被截斷時，cwd 仍正確更新。
6i. `shell_mode = "raw"` 的主機：命令原樣執行，輸出 `cwd` 為 null。
7. stdout 中沒有任何非 JSON-RPC 輸出（用 Inspector 檢查，或把 stdout 導到檔案檢查）。

---

## 11. 里程碑

| Phase | 內容 | 產出 | 預估 |
|---|---|---|---|
| **0. 技術驗證** | 確認 rmcp / russh 最新版 API；寫最小 PoC：stdio server 加一個 `echo` 工具；russh 透過 Windows agent 連線並執行一個命令 | PoC、鎖定版本 | 0.5–1 天 |
| **1. 核心 v0.1** | config、`ssh_list_hosts`、`ssh_exec`（含 timeout、截斷、exit code）、cwd 記憶、env / pre_command 包裝、known_hosts、audit、log | 可在 Claude Desktop 使用 | 3–4 天 |
| **2. 強化** | 連線池、keepalive、斷線重連、guard、`--check-config`、錯誤訊息 | v0.1 release | 1–2 天 |
| **3. 發佈** | 本機 release 建置、README、config 範例、SHA256 | `ssh-mcp.exe` | 0.5 天 |
| **4. v0.2** | SFTP upload/download、Pageant、per-host 命令白名單（選用） | v0.2 | 視需求 |

---

## 12. 風險與待確認事項

| # | 項目 | 影響 | 處理方式 |
|---|---|---|---|
| R1 | rmcp API 變動頻繁 | 升級時需要改程式碼 | 已鎖定 `=3.8.0` 並提交 `Cargo.lock`；SDK 相關程式碼集中在 `server.rs` |
| R2 | Windows agent 的實機行為 | 可能無法透過 agent 認證 | russh 0.65 API 已支援；Linux 環境已驗證 agent 認證流程。2026-10-30 已在 Windows 11 實機完成 MSVC 建置（fmt / clippy / 16 項單元測試通過、無 VCRUNTIME 相依、MCP 握手正常）。**透過 Windows OpenSSH Agent 實際連線遠端主機尚未驗證** |
| R3 | `aws-lc-rs` 的建置相依（CMake、NASM） | 建置失敗 | 已改用 `ring` 後端 |
| R4 | 遠端 timeout 後行程仍在執行 | 遠端殘留行程 | 文件說明；可選擇加上遠端 `timeout` 包裝 |
| R5 | 黑名單可被繞過 | 誤以為很安全 | 文件明確說明；強制要求 L1 低權限帳號 |
| R6 | SmartScreen / 防毒軟體攔截 | 使用者無法執行 | code signing 或 IT 白名單 |
| R7 | 非 UTF-8 輸出（例如 Big5 的舊系統） | 亂碼 | v0.1 使用 lossy 轉換；v0.2 可加 per-host `encoding` 設定 |
| R8 | 遠端登入 shell 不是 POSIX（fish、csh、Windows cmd/PowerShell） | 包裝 script 語法錯誤 | 該主機設 `shell_mode = "raw"`；`--check-config` 可加上偵測登入 shell 的選項 |
| R9 | 每次執行都跑 pre_command，若很慢（例如 source 大型 SDK 環境） | 每個命令都多等幾秒 | 文件說明；必要時改成只設定 `env`，或在遠端準備一個輕量的環境 script |
| R10 | 使用者命令回傳 254/253 被誤判為 server 錯誤 | 錯誤訊息誤導 | 同時比對 stderr 中帶 nonce 的錯誤標記（§8.2） |

---

## 13. 使用者端設定（預計）

1. 把 `ssh-mcp.exe` 放到例如 `C:\Tools\ssh-mcp\`。
2. 啟用 Windows OpenSSH Agent（PowerShell，系統管理員）：

   ```powershell
   Get-Service ssh-agent | Set-Service -StartupType Automatic
   Start-Service ssh-agent
   ssh-add $env:USERPROFILE\.ssh\id_ed25519
   ```

3. 先用 `ssh` 手動連一次每台主機（非 22 port 要加 `-p`，例如 `ssh -p 2222 pi@192.168.10.50`），讓 `known_hosts` 記下主機金鑰。
4. 建立 `%APPDATA%\ssh-mcp\config.toml`，參考 `config.example.toml`。
5. 執行 `ssh-mcp.exe --check-config` 驗證設定。
6. 在 Claude Desktop 的 `%APPDATA%\Claude\claude_desktop_config.json` 加入：

   ```json
   {
     "mcpServers": {
       "ssh": { "command": "C:\\Tools\\ssh-mcp\\ssh-mcp.exe" }
     }
   }
   ```

7. 從系統匣完全結束 Claude Desktop 再重新開啟。載入失敗時查看 `%APPDATA%\Claude\logs\mcp-server-ssh.log`。
