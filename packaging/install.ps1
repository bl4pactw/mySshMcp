<#
.SYNOPSIS
  ssh-mcp 手動安裝（zip 版）：複製 exe、建立設定檔範本、檢查 ssh-agent，並印出 Claude Desktop 設定片段。

.DESCRIPTION
  - 不會修改 claude_desktop_config.json（避免覆蓋你既有的 MCP 設定），只印出要加入的內容。
  - 已存在的 config.toml 不會被覆蓋。

.PARAMETER InstallDir
  exe 安裝位置，預設 C:\Tools\ssh-mcp

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\install.ps1
  powershell -ExecutionPolicy Bypass -File .\install.ps1 -InstallDir D:\Apps\ssh-mcp
#>
[CmdletBinding()]
param(
    [string]$InstallDir = 'C:\Tools\ssh-mcp'
)

$ErrorActionPreference = 'Stop'
$Here = $PSScriptRoot

function Ok($m)   { Write-Host "  [OK] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "  [!]  $m" -ForegroundColor Yellow }

Write-Host '==> 安裝 ssh-mcp.exe' -ForegroundColor Cyan
New-Item -ItemType Directory -Force $InstallDir | Out-Null
$Exe = Join-Path $InstallDir 'ssh-mcp.exe'
try {
    Copy-Item (Join-Path $Here 'ssh-mcp.exe') $Exe -Force
} catch {
    throw "無法覆蓋 $Exe（可能正在被 Claude Desktop 使用）。請先完全結束 Claude Desktop 再執行。`n$_"
}
Ok "$Exe（$((& $Exe --version).Trim())）"

Write-Host '==> 設定檔' -ForegroundColor Cyan
# 注意：一定要在一般 PowerShell 執行（不是從 Microsoft Store 版 Claude 內），才會寫到真正的 %APPDATA%
$CfgDir = Join-Path $env:APPDATA 'ssh-mcp'
$Cfg    = Join-Path $CfgDir 'config.toml'
New-Item -ItemType Directory -Force $CfgDir | Out-Null
if (Test-Path $Cfg) {
    Ok "已存在，不覆蓋：$Cfg"
} else {
    Copy-Item (Join-Path $Here 'config.example.toml') $Cfg
    Warn "已建立範本：$Cfg —— 請編輯主機清單"
}

Write-Host '==> ssh-agent' -ForegroundColor Cyan
$svc = Get-Service ssh-agent -ErrorAction SilentlyContinue
if (-not $svc) {
    Warn '找不到 ssh-agent 服務：請在「設定 > 系統 > 選用功能」安裝 OpenSSH 用戶端'
} elseif ($svc.Status -ne 'Running') {
    Warn "ssh-agent 未執行（$($svc.Status)）。請以系統管理員執行："
    Write-Host '        Get-Service ssh-agent | Set-Service -StartupType Automatic; Start-Service ssh-agent'
} else {
    # PowerShell 5.1 在 ErrorActionPreference=Stop 時，原生命令的 stderr 重導會變成例外，所以暫時放寬
    $prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    $keys = & ssh-add -l 2>&1 | ForEach-Object { "$_" }
    $rc = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($rc -eq 0) { Ok "ssh-agent 執行中，已載入金鑰：`n$($keys -join "`n")" }
    else { Warn '已執行但沒有金鑰：請執行 ssh-add $env:USERPROFILE\.ssh\id_ed25519' }
}

Write-Host ''
Write-Host '==> 下一步' -ForegroundColor Cyan
Write-Host "  1. 編輯 $Cfg"
Write-Host '  2. 每台主機先用 ssh 手動連線一次，記錄主機金鑰（非 22 port 加 -p）'
Write-Host "  3. 檢查：& '$Exe' --check-config --connect"
Write-Host '  4. 把下面的內容加進 claude_desktop_config.json 的 "mcpServers"（Claude Desktop：設定 > 開發者 > 編輯設定）：'
$escaped = $Exe.Replace('\', '\\')
Write-Host ''
Write-Host "    `"ssh`": { `"command`": `"$escaped`" }" -ForegroundColor White
Write-Host ''
Write-Host '  5. 從系統匣完全結束 Claude Desktop 後重新開啟'
