# ssh-mcp 安裝與 Claude Desktop 設定

有兩種安裝方式，**擇一**即可，不要兩種同時裝（Claude 會看到兩組相同的工具）：

| | A. 擴充功能（.mcpb）**建議** | B. 手動（zip） |
|---|---|---|
| 檔案 | `ssh-mcp-<ver>.mcpb` | `ssh-mcp-<ver>-win-x64.zip` |
| 安裝 | 在 Claude Desktop 中安裝，exe 由 Claude 管理 | 自己放 exe、編輯 `claude_desktop_config.json` |
| 設定 | 擴充功能設定頁可選設定檔、開關危險命令、調 log 等級 | 寫在 JSON 的 `env` 裡 |
| 更新 | 安裝新版 .mcpb 即可 | 結束 Claude 後覆蓋 exe |
| 限制 | 組織管理員可能停用自訂擴充功能 | 無 |

> ⚠ 安裝前請先理解：這等同把 SSH 權限交給 Claude。遠端請使用**專用低權限帳號**，並讓 `ssh_exec` 維持「每次詢問」，不要設成自動允許。

> 💡 想請自己的 Claude 帶著安裝：把這個檔案交給它，並說「請依照 INSTALL.md §5 協助我安裝 ssh-mcp」。§5 是寫給 AI 助理的執行指引，標明了哪些步驟它可以做、哪些必須由你本人操作。

---

## 0. 打包（開發者）

在專案根目錄：

```powershell
powershell -ExecutionPolicy Bypass -File packaging\package.ps1
```

- 會執行 `cargo build --release`，再產生 `dist\ssh-mcp-<ver>.mcpb`、`dist\ssh-mcp-<ver>-win-x64.zip`、`dist\SHA256SUMS.txt`。
- 已經建置好時加 `-NoBuild`；記憶體不足時先 `cargo build --release -j 4`，再 `package.ps1 -NoBuild`。
- 有 Node.js（`npx`）時會用官方 `@anthropic-ai/mcpb` 工具驗證 manifest 並打包；沒有時直接用 .NET 產生 zip 格式的 .mcpb。
- 版本號取自 `Cargo.toml`，manifest 會自動同步。

---

## 1. 共同前置作業（兩種方式都要）

### 1.1 ssh-agent 與金鑰

以系統管理員身分開啟 PowerShell：

```powershell
Get-Service ssh-agent | Set-Service -StartupType Automatic
Start-Service ssh-agent
```

以一般使用者身分：

```powershell
ssh-add $env:USERPROFILE\.ssh\id_ed25519
ssh-add -l
```

使用 PuTTY Pageant 的話，在設定檔設 `agent = "pageant"`。

### 1.2 記錄主機金鑰

ssh-mcp 不會自動信任新主機。每台主機先手動連線一次，確認指紋：

```powershell
ssh builder@build-01.lan
ssh -p 2222 pi@192.168.10.50
```

### 1.3 設定檔

在**一般 PowerShell 或檔案總管**中建立 `%APPDATA%\ssh-mcp\config.toml`，內容參考 `config.example.toml`（zip 與 .mcpb 中都有附）：

```powershell
New-Item -ItemType Directory -Force $env:APPDATA\ssh-mcp | Out-Null
notepad $env:APPDATA\ssh-mcp\config.toml
```

> **Microsoft Store（MSIX）版 Claude Desktop**：封裝 App 啟動的 ssh-mcp 寫入 `%APPDATA%` 時，會被導到 `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\`。因此設定檔一定要在 Claude 以外建立（封裝 App 讀得到真正的 `%APPDATA%`，反過來則看不到）。稽核記錄與 log 想固定位置時，在設定檔把 `log_dir`、`output_dir` 設到 `%APPDATA%` 以外，例如 `C:\Tools\ssh-mcp\logs`。

### 1.4 檢查

```powershell
<ssh-mcp.exe 路徑> --check-config --connect
```

每台主機都顯示 ✓ 再繼續。用 .mcpb 安裝的話，可以先解壓 zip 版的 exe 來檢查，或在 Claude 中直接請它執行 `ssh_list_hosts`。

---

## 2A. 安裝擴充功能（.mcpb）

1. 開啟 Claude Desktop → **設定（Settings）→ 擴充功能（Extensions）**。
2. 把 `ssh-mcp-<ver>.mcpb` **拖進視窗**，或直接**雙擊** .mcpb 檔；也可以在擴充功能頁的進階設定中選「安裝擴充功能」。
3. 確認安裝畫面上的名稱、版本與工具清單（`ssh_list_hosts`、`ssh_exec`），按 **安裝**。
4. 在擴充功能設定頁調整：

   | 欄位 | 說明 |
   |---|---|
   | 設定檔（config.toml） | 留空 = `%APPDATA%\ssh-mcp\config.toml`；要用其他位置時選擇檔案 |
   | 允許危險命令 | 預設關閉。開啟後，只有設定檔中 `allow_dangerous = true` 的主機能執行黑名單命令 |
   | Log 等級 | `error` / `warn` / `info` / `debug` |

5. 確認擴充功能是**啟用**狀態。

**修改設定檔之後**：在擴充功能頁把 SSH MCP 停用再啟用（或重開 Claude Desktop），設定才會重新載入。

**更新版本**：安裝新版 .mcpb 即可覆蓋；擴充功能設定會保留。

**移除**：設定 → 擴充功能 → SSH MCP → 解除安裝。`%APPDATA%\ssh-mcp\` 下的設定檔與記錄不會被刪除。

---

## 2B. 手動安裝（zip）

1. 解壓 `ssh-mcp-<ver>-win-x64.zip`。
2. 在解壓目錄執行（預設安裝到 `C:\Tools\ssh-mcp`）：

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\install.ps1
   # 或指定位置：.\install.ps1 -InstallDir D:\Apps\ssh-mcp
   ```

   它會複製 exe、在設定檔不存在時建立範本、檢查 ssh-agent，並印出要加入的 JSON。**不會**修改你的 `claude_desktop_config.json`。

3. 開啟 Claude Desktop → **設定 → 開發者（Developer）→ 編輯設定（Edit Config）**，在 `mcpServers` 中加入：

   ```json
   {
     "mcpServers": {
       "ssh": {
         "command": "C:\\Tools\\ssh-mcp\\ssh-mcp.exe"
       }
     }
   }
   ```

   已經有其他 MCP server（例如 `adb-embedded`）時，把 `"ssh": {...}` 加在同一個 `mcpServers` 物件中，用逗號分隔。

   可選的環境變數：

   ```json
   "ssh": {
     "command": "C:\\Tools\\ssh-mcp\\ssh-mcp.exe",
     "env": {
       "SSH_MCP_CONFIG": "D:\\configs\\ssh-mcp.toml",
       "SSH_MCP_ALLOW_DANGEROUS": "1",
       "SSH_MCP_LOG": "debug"
     }
   }
   ```

4. 從系統匣**完全結束** Claude Desktop（不是只關視窗），再重新開啟。

**更新版本**：完全結束 Claude Desktop → 重新執行新版的 `install.ps1`（會覆蓋 exe，保留設定檔）→ 重開。

---

## 3. 確認可以使用

1. 開新對話，在輸入框的工具／連接器選單中應該看到 **ssh**（或 SSH MCP）與兩個工具。
2. 輸入：「列出可用的 SSH 主機」→ Claude 會呼叫 `ssh_list_hosts`。
3. 輸入：「在 build-01 上執行 uname -a」→ 會跳出 `ssh_exec` 的允許確認，選 **允許一次**。

建議的權限設定：

| 工具 | 建議 |
|---|---|
| `ssh_list_hosts` | 可以設成一律允許（唯讀） |
| `ssh_exec` | 維持**每次詢問** |

---

## 4. 記錄與疑難排解

| 位置 | 內容 |
|---|---|
| `%APPDATA%\Claude\logs\mcp*.log` | Claude Desktop 啟動 MCP server 的記錄（載入失敗先看這裡）。MSIX 版在 `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\Claude\logs\` |
| `%APPDATA%\ssh-mcp\logs\audit-YYYYMMDD.jsonl` | 每次工具呼叫的稽核記錄 |
| `%APPDATA%\ssh-mcp\logs\ssh-mcp.log.YYYY-MM-DD` | ssh-mcp 執行 log |
| `%APPDATA%\ssh-mcp\outputs\` | 輸出被截斷時的完整內容 |

（MSIX 版 Claude 啟動時，後三項會在 `LocalCache\Roaming\ssh-mcp\` 下，見 §1.3。）

| 症狀 | 處理方式 |
|---|---|
| 工具選單中看不到 ssh | 確認擴充功能已啟用／JSON 格式正確（逗號、跳脫的 `\\`）；完全結束 Claude 後重開；查看 `mcp*.log` |
| 無法讀取設定檔 … (os error 3) | 設定檔不在預期位置；MSIX 版請確認檔案在真正的 `%APPDATA%\ssh-mcp\`（§1.3） |
| 無法連線 Windows OpenSSH Agent | `Start-Service ssh-agent` |
| ssh-agent 中沒有可用金鑰 | `ssh-add <私鑰>` |
| known_hosts 中沒有 … 的主機金鑰 | 先用 `ssh` 手動連線一次（非 22 port 加 `-p`） |
| 主機金鑰與 known_hosts 第 N 行不符 | 確認主機是否重灌；確定安全後 `ssh-keygen -R "[host]:port"` 再重新記錄 |
| 金鑰都不被接受 | 公鑰需在遠端帳號的 `~/.ssh/authorized_keys` |
| 命令被安全規則擋下 | 預期行為。確實需要時：該主機設 `allow_dangerous = true`，並開啟擴充功能的「允許危險命令」（或 env `SSH_MCP_ALLOW_DANGEROUS=1`） |
| 安裝 .mcpb 時顯示組織不允許 | 公司管理員停用了自訂擴充功能，改用方式 B |
| 覆蓋 exe 時「檔案正在使用中」 | 完全結束 Claude Desktop 後再試 |
| Windows SmartScreen 警告 | 未簽章 exe 的正常現象；內部使用可請 IT 加入白名單 |

---

## 5. 給 AI 助理的安裝指引

> 本節寫給協助使用者安裝的 AI 助理（Claude Code、Claude Desktop／Cowork 等）。使用者請直接把本檔交給 AI，並說「請依照 INSTALL.md §5 協助我安裝 ssh-mcp」。

### 5.1 先判斷你能做什麼

| 你的環境 | 判斷方式 | 你能做的 |
|---|---|---|
| **能在使用者的 Windows 上執行 PowerShell**（例如 Windows 上的 Claude Code） | `$PSVersionTable.PSVersion` 有結果，且 `$env:OS` 為 `Windows_NT` | 執行 §5.4 中標示 🤖 的步驟 |
| **只能在 Linux 環境執行命令**（例如 Cowork 的命令環境）或**完全不能執行命令**（一般聊天） | `uname` 回傳 Linux，或沒有執行工具 | 不要嘗試執行 Windows 命令。改成：產生 config.toml 內容（有連接資料夾時直接寫入）、把 🤖 步驟的命令整理成可複製的區塊讓使用者貼到 PowerShell、解讀使用者貼回的輸出 |

不論哪種環境，標示 👤 的步驟都**必須由使用者本人操作**；輪到時請停下來，說明要做什麼、為什麼要本人做，等使用者回報後再繼續。

### 5.2 禁止事項（即使使用者要求也先說明風險並取得明確同意）

1. **不要替使用者信任主機金鑰**：不要使用 `StrictHostKeyChecking=no` / `accept-new`，不要執行 `ssh-keyscan >> known_hosts`，不要編輯 `known_hosts`。主機金鑰指紋必須由使用者本人比對確認。
2. **不要經手私鑰與密碼**：不要讀取 `~/.ssh/id_*`、不要詢問或代為輸入金鑰的 passphrase、不要在設定檔中寫入任何密碼。
3. **不要提高權限**：不要以系統管理員身分執行命令，不要觸發 UAC；需要管理員權限的步驟交給使用者。
4. **不要放寬安全設定**：不要設定 `allow_dangerous = true`、`SSH_MCP_ALLOW_DANGEROUS`，不要建議把 `ssh_exec` 設成自動允許。使用者主動要求時，先說明這等於讓 Claude 不經確認就能執行 reboot、mkfs 等命令。
5. **不要破壞既有設定**：不要覆蓋已存在的 `config.toml`；編輯 `claude_desktop_config.json` 前先備份，且只新增 `mcpServers.ssh`，保留其他 server。
6. **不要結束或重開 Claude Desktop**：如果你本身跑在 Claude Desktop 裡，這會中斷你自己的工作；交給使用者。
7. **不要同時安裝兩種方式**（.mcpb 與手動 JSON 設定擇一）。

### 5.3 開始前向使用者確認

一次問清楚以下資訊，不要猜：

- 安裝方式：A（.mcpb 擴充功能）或 B（zip 手動）；不確定時建議 A，公司帳號無法安裝自訂擴充功能時改用 B。
- 安裝檔位置：`.mcpb` 或 zip 的路徑。
- 方式 B 的安裝目錄（預設 `C:\Tools\ssh-mcp`）。
- 使用 Windows OpenSSH Agent 還是 Pageant。
- 要連線的主機清單：alias、host、port（非 22 時）、帳號；選填：初始目錄 `cwd`、`pre_command`、`env`。
- Claude Desktop 是否為 Microsoft Store（MSIX）版本：`Get-AppxPackage *Claude*` 有結果即是。

### 5.4 步驟

🤖 = AI 可執行（能在 Windows 執行 PowerShell 時）　👤 = 必須由使用者操作

| # | 執行者 | 步驟 | 命令／動作 | 成功判斷 |
|---|---|---|---|---|
| 1 | 🤖 | 確認 OpenSSH 用戶端 | `Get-Command ssh, ssh-add` | 兩者都找得到 |
| 1a | 👤 | （找不到時）安裝 OpenSSH 用戶端 | 設定 → 系統 → 選用功能 → 新增「OpenSSH 用戶端」 | 重跑步驟 1 成功 |
| 2 | 🤖 | 檢查 ssh-agent 服務 | `Get-Service ssh-agent \| Select Status, StartType` | `Running` / `Automatic`（使用 Pageant 則略過） |
| 2a | 👤 | （未執行時）以**系統管理員** PowerShell 啟動 | `Get-Service ssh-agent \| Set-Service -StartupType Automatic; Start-Service ssh-agent` | 重跑步驟 2 成功 |
| 3 | 🤖 | 檢查金鑰 | `ssh-add -l` | exit code 0，列出至少一把金鑰 |
| 3a | 👤 | （沒有金鑰時）載入金鑰，密碼由本人輸入 | `ssh-add $env:USERPROFILE\.ssh\id_ed25519` | 重跑步驟 3 成功 |
| 4A | 👤 | 方式 A：安裝擴充功能 | 設定 → 擴充功能，把 `.mcpb` 拖進視窗或雙擊，確認後按「安裝」；設定欄位維持預設（設定檔留空、危險命令關閉） | 擴充功能清單中出現 SSH MCP 且為啟用 |
| 4B | 🤖 | 方式 B：執行安裝腳本 | 在解壓目錄：`powershell -ExecutionPolicy Bypass -File .\install.ps1 [-InstallDir <dir>]` | 輸出 `[OK] ...ssh-mcp.exe（ssh-mcp x.y.z）`；若顯示「無法覆蓋」→ 請使用者完全結束 Claude Desktop 後重試 |
| 5 | 🤖 | 建立 `config.toml` | 依步驟 5.3 收集的資訊，以 `config.example.toml` 為範本寫入 `%APPDATA%\ssh-mcp\config.toml`。**檔案已存在時不要覆蓋**：把建議的修改給使用者確認後再改，改之前先備份 | 檔案存在且內容與使用者提供的主機清單一致 |
| 6 | 🤖 | 驗證設定檔語法 | `<exe> --check-config`（不加 `--connect`） | exit code 0，列出每台主機的最終 host / port / user，且顯示 `✓ ssh-agent` |
| 7 | 👤 | 記錄主機金鑰 | 對每台主機：`ssh -p <port> <user>@<host>`，**本人比對指紋**後回答 yes，登入成功即可離開。AI 可以把每台主機的命令列成清單 | 每台都能登入 |
| 8 | 🤖 | 連線測試 | `<exe> --check-config --connect` | 每台主機都顯示 ✓，exit code 0 |
| 9B | 🤖 | 方式 B：加入 Claude Desktop 設定 | 見 §5.5 | JSON 有效，`mcpServers.ssh.command` 指向 exe |
| 10 | 👤 | 重開 Claude Desktop | 從系統匣完全結束（不是只關視窗），再重新開啟 | — |
| 11 | 👤 | 在 Claude 中確認 | 開新對話輸入「列出可用的 SSH 主機」；`ssh_exec` 的權限維持「每次詢問」 | Claude 呼叫 `ssh_list_hosts` 並列出主機 |

**步驟 6、8 的 `<exe>`**：

- 方式 B：`C:\Tools\ssh-mcp\ssh-mcp.exe`（或使用者指定的安裝目錄）。
- 方式 A：exe 位於 Claude 的擴充功能目錄中，可用以下命令尋找；找不到時，請使用者另外解壓 zip 版的 exe 來做檢查：

  ```powershell
  Get-ChildItem $env:APPDATA\Claude, "$env:LOCALAPPDATA\Packages\Claude_*\LocalCache\Roaming\Claude" `
    -Recurse -Filter ssh-mcp.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName
  ```

### 5.5 方式 B：安全地合併 `claude_desktop_config.json`

1. **確認檔案位置**：請使用者在 Claude Desktop 打開「設定 → 開發者 → 編輯設定」，用它開啟的檔案路徑為準。一般版通常是 `%APPDATA%\Claude\claude_desktop_config.json`；MSIX 版可能在 `%LOCALAPPDATA%\Packages\Claude_<id>\LocalCache\Roaming\Claude\` 下。
2. **以 JSON 解析後合併**，不要用文字取代。先備份；已有 `ssh` 項目時停下來問使用者：

   ```powershell
   $cfg = "$env:APPDATA\Claude\claude_desktop_config.json"   # 依步驟 1 確認的路徑修改
   $exe = 'C:\Tools\ssh-mcp\ssh-mcp.exe'                     # 依實際安裝目錄修改

   if (Test-Path $cfg) {
       Copy-Item $cfg "$cfg.bak-$(Get-Date -Format yyyyMMdd-HHmmss)"
       $j = Get-Content $cfg -Raw -Encoding UTF8 | ConvertFrom-Json
   } else {
       $j = [pscustomobject]@{}
   }
   if (-not $j.PSObject.Properties['mcpServers']) {
       $j | Add-Member -NotePropertyName mcpServers -NotePropertyValue ([pscustomobject]@{})
   }
   if ($j.mcpServers.PSObject.Properties['ssh']) {
       throw '已經有名為 ssh 的 MCP server 設定，請使用者確認是否要取代'
   }
   $j.mcpServers | Add-Member -NotePropertyName ssh -NotePropertyValue ([pscustomobject]@{ command = $exe })
   [IO.File]::WriteAllText($cfg, ($j | ConvertTo-Json -Depth 32), (New-Object Text.UTF8Encoding $false))
   Get-Content $cfg -Raw -Encoding UTF8 | ConvertFrom-Json | Out-Null   # 確認寫回後仍是有效 JSON
   ```

3. 告訴使用者備份檔的位置，然後進行步驟 10（請使用者重開 Claude Desktop）。

### 5.6 出錯時

- 先看命令的完整錯誤訊息，對照 §4 的疑難排解表。
- `--check-config` 的錯誤訊息已經說明原因與處理方式（例如要執行 `ssh-add`、要先手動 ssh 一次），照著告訴使用者即可；屬於 👤 的處理方式不要自己執行。
- Claude Desktop 看不到工具時，請使用者提供 `%APPDATA%\Claude\logs\` 下 `mcp` 開頭的 log（MSIX 版路徑見 §4）。

### 5.7 完成時的回報

用一張清單回報：

- ✅ 已完成的步驟（安裝位置、設定檔位置、`claude_desktop_config.json` 備份檔位置）。
- 👤 還需要使用者做的步驟（通常是 10、11）。
- 連線測試結果：哪些主機成功、哪些失敗與原因。回報時用 alias 指稱主機即可，不需要重複列出 IP。
