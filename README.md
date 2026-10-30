# ssh-mcp

個人用 SSH MCP server（STDIO transport），Windows x86_64 單一執行檔。
讓 Claude 等 MCP client 透過 SSH 在**設定檔指定的主機**上執行命令並取回結果。

- 設計與決策：見 [PLAN.md](PLAN.md)
- 技術：Rust + [rmcp](https://crates.io/crates/rmcp) 3.8.0 + [russh](https://crates.io/crates/russh) 0.65.0（ring 加密後端）
- 工具：`ssh_list_hosts`（唯讀）、`ssh_exec`（⚠ 會改變遠端狀態，client 應逐次確認）

> ⚠ 這個程式等同把 SSH 權限交給 LLM。請務必：遠端使用**專用低權限帳號**、MCP client **不要設成自動核准** `ssh_exec`。

---

## 1. 建置（Windows）

### 1.1 安裝工具（只需一次）

1. **Visual Studio Build Tools**：安裝時勾選「使用 C++ 的桌面開發」（Desktop development with C++）。
2. **Rust**：到 <https://rustup.rs> 下載 `rustup-init.exe`，使用預設選項（`x86_64-pc-windows-msvc`）。
3. 開新的終端機確認：

   ```powershell
   cargo --version     # 需要 1.89 以上
   ```

### 1.2 編譯

第一次建置前，把 `cargo-config.toml` 複製成 `.cargo\config.toml`（靜態連結 C runtime，讓 exe 不依賴 VC++ Redistributable）：

```powershell
cd C:\Users\user\playground\mySshMcp
New-Item -ItemType Directory -Force .cargo | Out-Null
Copy-Item cargo-config.toml .cargo\config.toml
cargo build --release
```

> 建置時出現 `memory allocation … failed` 或 `os error 1455`（分頁檔太小），代表並行編譯耗盡記憶體：先 `cargo clean`，再加 `-j 4` 重建（例如 `cargo build --release -j 4`）。

產出 `target\release\ssh-mcp.exe`。這是單一檔案，複製到任何 Windows x64 電腦都能直接執行，不需要安裝其他東西。

發佈前建議執行：

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test
dumpbin /dependents target\release\ssh-mcp.exe     # 不應出現 VCRUNTIME140.dll（需在 Developer PowerShell 中執行）
certutil -hashfile target\release\ssh-mcp.exe SHA256
```

### 1.3 打包

```powershell
powershell -ExecutionPolicy Bypass -File packaging\package.ps1          # 建置 + 打包
powershell -ExecutionPolicy Bypass -File packaging\package.ps1 -NoBuild # 只打包現有的 exe
```

產出到 `dist\`：

| 檔案 | 用途 |
|---|---|
| `ssh-mcp-<ver>.mcpb` | Claude Desktop 擴充功能（設定 → 擴充功能，拖入或雙擊安裝） |
| `ssh-mcp-<ver>-win-x64.zip` | 手動安裝：exe、`config.example.toml`、`install.ps1`、`INSTALL.md` |
| `SHA256SUMS.txt` | exe、.mcpb、zip 的 SHA256 |

擴充功能的定義在 `packaging\manifest.json`（版本號由打包腳本從 `Cargo.toml` 同步）。安裝與 Claude Desktop 設定步驟見 **[INSTALL.md](INSTALL.md)**。

---

## 2. 使用者設定

> 這一節是手動安裝（zip）的步驟摘要；擴充功能（.mcpb）安裝方式與完整說明見 [INSTALL.md](INSTALL.md)。

### 2.1 放置執行檔

例如 `C:\Tools\ssh-mcp\ssh-mcp.exe`。

### 2.2 啟用 Windows OpenSSH Agent 並載入金鑰

以系統管理員身分開啟 PowerShell：

```powershell
Get-Service ssh-agent | Set-Service -StartupType Automatic
Start-Service ssh-agent
```

以一般使用者身分：

```powershell
ssh-add $env:USERPROFILE\.ssh\id_ed25519
ssh-add -l          # 確認已載入
```

> ssh-mcp 不讀取私鑰檔，也不支援密碼登入；一律透過 agent 簽章。使用 PuTTY Pageant 時，在設定檔設 `agent = "pageant"`。

### 2.3 記錄主機金鑰

ssh-mcp **不會自動信任新主機**。每台主機先手動連線一次，確認指紋後讓 `known_hosts` 記下來：

```powershell
ssh builder@build-01.lan
ssh -p 2222 pi@192.168.10.50      # 非 22 port
```

### 2.4 建立設定檔

把 [`config.example.toml`](config.example.toml) 複製到 `%APPDATA%\ssh-mcp\config.toml` 並修改。

重點欄位：

| 欄位 | 說明 |
|---|---|
| `[defaults]` | 所有主機共用的 port、user、cwd、pre_command、env |
| `[[hosts]]` | `alias`、`host` 必填；其他欄位會覆蓋 `[defaults]`；port 都沒設時用 22 |
| `cwd` | 初始工作目錄（絕對路徑）。之後 LLM 下的 `cd` 會被記住 |
| `pre_command` | 每次執行前先跑，例如 `source oe-init-build-env`；失敗時命令不會執行 |
| `env` | 每次執行都 export 的環境變數 |
| `shell_mode` | `posix`（預設）；登入 shell 不是 POSIX 的主機用 `raw` |
| `allow_dangerous` | 搭配環境變數 `SSH_MCP_ALLOW_DANGEROUS=1` 才會放行黑名單命令 |

### 2.5 檢查設定

```powershell
C:\Tools\ssh-mcp\ssh-mcp.exe --check-config            # 顯示套用預設值後的最終設定、檢查 agent
C:\Tools\ssh-mcp\ssh-mcp.exe --check-config --connect  # 另外實際連線每台主機執行 echo ok
```

### 2.6 加入 Claude Desktop

編輯 `%APPDATA%\Claude\claude_desktop_config.json`：

```json
{
  "mcpServers": {
    "ssh": {
      "command": "C:\\Tools\\ssh-mcp\\ssh-mcp.exe"
    }
  }
}
```

需要放行危險命令的主機（同時要在該主機設定 `allow_dangerous = true`；值可為 `1` 或 `true`）：

```json
"ssh": {
  "command": "C:\\Tools\\ssh-mcp\\ssh-mcp.exe",
  "env": { "SSH_MCP_ALLOW_DANGEROUS": "1" }
}
```

從系統匣**完全結束** Claude Desktop 再重新開啟。

---

## 3. 運作方式

### 3.1 每次執行都是新的 shell

- SSH 連線會保留重用（連線池），不需要每次重新認證。
- 每次 `ssh_exec` 都開新 channel、啟動新的遠端 shell，執行完就結束。
- **工作目錄會被記住**：server 在每個命令結束時用 `pwd` 取得目錄，下一次先 `cd` 回去。
- **環境變數不會保留**：固定環境請用設定檔的 `env` / `pre_command`。
- 不支援互動式命令（vim、top、需要輸入密碼的 sudo 等）；stdin 會立即收到 EOF。

實際送到遠端的 script（`posix` 模式）大致如下：

```sh
export LANG='C.UTF-8'
cd -- '<記住的目錄>' || exit 254
__sshmcp_cwd=$(pwd)
{ <pre_command>
} || exit 253
cd -- "$__sshmcp_cwd"
<使用者命令>
__sshmcp_rc=$?
printf '\n__SSHMCP_<nonce>_CWD=%s\n' "$(pwd)"
exit $__sshmcp_rc
```

### 3.2 `ssh_exec` 參數與回傳

| 參數 | 說明 |
|---|---|
| `host` | 主機 alias（必填） |
| `command` | shell 命令，可多行（必填） |
| `timeout_s` | 逾時秒數，上限為 `max_timeout_s` |
| `cwd` | 本次改在此目錄執行（可為相對路徑），成功後成為新的記住目錄 |
| `reset_cwd` | `true` = 先回到設定檔的 cwd（沒設定則為登入目錄） |

回傳：`exit_code`、`exit_signal`、`stdout`、`stderr`、`cwd`、`cwd_updated`、`truncated`、`full_output_path`、`duration_ms`、`timed_out`。

- 命令中途 `exit`、被 signal 終止或逾時時，`cwd_updated = false`，沿用原本的目錄。
- 輸出超過 `max_output_bytes` 時保留頭尾，完整輸出存到 `output_dir`，路徑放在 `full_output_path`。
- 逾時只會關閉 channel，**遠端行程不一定會被終止**。長時間任務可在命令中自行加上 `timeout N`。

---

## 4. 檔案位置

| 用途 | 預設位置 |
|---|---|
| 設定檔 | `%APPDATA%\ssh-mcp\config.toml` |
| 稽核記錄 | `%APPDATA%\ssh-mcp\logs\audit-YYYYMMDD.jsonl` |
| 執行 log | `%APPDATA%\ssh-mcp\logs\ssh-mcp.log.YYYY-MM-DD`（等級用環境變數 `SSH_MCP_LOG` 調整，例如 `debug`） |
| 截斷時的完整輸出 | `%APPDATA%\ssh-mcp\outputs\` |
| Claude Desktop 載入失敗的 log | `%APPDATA%\Claude\logs\mcp-server-ssh.log` |

> **Microsoft Store / MSIX 版 Claude Desktop**：封裝 App 及其子行程（包括它啟動的 ssh-mcp）寫入 `%APPDATA%` 時，會被導到 `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\`。因此：
> - 設定檔請在**一般 PowerShell / 檔案總管**中建立，放在真正的 `%APPDATA%\ssh-mcp\`（封裝 App 讀得到；反過來則一般終端機看不到）。
> - 由 Claude Desktop 啟動時產生的稽核記錄、log、完整輸出會在上述 `LocalCache\Roaming\ssh-mcp\` 下。想固定位置可在設定檔把 `log_dir`、`output_dir` 設到 `%APPDATA%` 以外的目錄（例如 `C:\Tools\ssh-mcp\logs`）。

---

## 5. 疑難排解

| 訊息 | 處理方式 |
|---|---|
| 無法連線 Windows OpenSSH Agent | `Start-Service ssh-agent`；確認服務啟動類型為自動 |
| ssh-agent 中沒有可用金鑰 | `ssh-add <私鑰>`，再用 `ssh-add -l` 確認 |
| known_hosts 中沒有 … 的主機金鑰 | 先用 `ssh`（非 22 port 加 `-p`）手動連線一次 |
| ⚠ 主機金鑰與 known_hosts 第 N 行不符 | 確認主機是否重灌；確定安全後 `ssh-keygen -R "[host]:port"` 再重新連線記錄 |
| 金鑰都不被接受 | 確認公鑰已在遠端該帳號的 `~/.ssh/authorized_keys` |
| pre_command 執行失敗 | 在終端機用同一個帳號 ssh 進去手動執行 pre_command 檢查；dash 請用 `.` 取代 `source` |
| 無法進入工作目錄 | 目錄被刪除或沒有權限；讓 LLM 用 `reset_cwd: true` |
| 無法讀取設定檔 … (os error 3)，但檔案明明存在 | 檔案可能被 MSIX 版 Claude 寫到 `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\` 下（見 §4），請移到真正的 `%APPDATA%\ssh-mcp\` |
| Windows SmartScreen 警告 | 未簽章執行檔的正常現象；內部使用可請 IT 加入白名單 |

---

## 6. 開發與測試

```text
src/
├── main.rs          進入點、CLI（--version / --check-config）、log 初始化
├── config.rs        設定檔載入、[defaults] 合併、驗證
├── server.rs        MCP 工具（ssh_list_hosts / ssh_exec）
├── guard.rs         命令黑名單
├── audit.rs         稽核記錄（JSONL）
├── output.rs        截斷與完整輸出存檔
└── ssh/
    ├── client.rs    russh 連線、known_hosts 驗證、ssh-agent 認證
    ├── pool.rs      連線池（重用、閒置回收、斷線重連）
    ├── exec.rs      開 channel 執行命令
    ├── wrap.rs      包裝 script 產生與輸出解析
    └── state.rs     每個 alias 記住的 cwd
tests/e2e/
├── setup_linux.sh   在 Linux 建立測試用 sshd（port 2201 / 2222）、帳號、ssh-agent、設定檔
└── run_e2e.py       以 JSON-RPC 驅動 ssh-mcp，跑 PLAN §10 驗收案例
```

單元測試（任何平台）：

```sh
cargo test
```

端對端測試（Linux 或 WSL，需要 root；會建立 builder / pi / dashy 測試帳號）：

```sh
cargo build
sudo bash tests/e2e/setup_linux.sh /tmp/ssh-mcp-e2e
source /tmp/ssh-mcp-e2e/agent.env
python3 tests/e2e/run_e2e.py target/debug/ssh-mcp /tmp/ssh-mcp-e2e
```

協定層也可以用 MCP Inspector 手動測試：

```powershell
npx @modelcontextprotocol/inspector C:\Tools\ssh-mcp\ssh-mcp.exe
```
