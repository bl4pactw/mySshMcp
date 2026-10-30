<#
.SYNOPSIS
  建置並打包 ssh-mcp：產出 .mcpb（Claude Desktop 擴充功能）與 zip（手動安裝）。

.DESCRIPTION
  輸出到 dist\：
    ssh-mcp-<ver>.mcpb          在 Claude Desktop 中安裝的擴充功能套件
    ssh-mcp-<ver>-win-x64.zip   exe + 範例設定 + install.ps1 + INSTALL.md
    SHA256SUMS.txt              上述檔案與 exe 的 SHA256

  有 Node.js（npx）時使用官方 @anthropic-ai/mcpb 工具驗證並打包；
  沒有時以 .NET ZipFile 直接產生（.mcpb 本質上就是 zip，manifest.json 在根目錄）。

.PARAMETER NoBuild
  跳過 cargo build，直接使用現有的 target\release\ssh-mcp.exe

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\package.ps1
#>
[CmdletBinding()]
param(
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root    = Split-Path -Parent $PSScriptRoot
$PkgDir  = $PSScriptRoot
$Dist    = Join-Path $Root 'dist'
$Stage   = Join-Path $Dist 'stage'
$Exe     = Join-Path $Root 'target\release\ssh-mcp.exe'
$McpbCli = '@anthropic-ai/mcpb@2.1.2'
$Utf8NoBom = New-Object System.Text.UTF8Encoding $false

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

Set-Location $Root

# ---- 版本號：取自 Cargo.toml ----
$cargoToml = Get-Content (Join-Path $Root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Cargo.toml 中找不到 version' }
$Version = $Matches[1]
Step "ssh-mcp $Version"

# ---- 建置 ----
if (-not $NoBuild) {
    $cargoCfg = Join-Path $Root '.cargo\config.toml'
    if (-not (Test-Path $cargoCfg)) {
        Step '建立 .cargo\config.toml（靜態連結 C runtime）'
        New-Item -ItemType Directory -Force (Split-Path $cargoCfg) | Out-Null
        Copy-Item (Join-Path $Root 'cargo-config.toml') $cargoCfg
    }
    Step 'cargo build --release'
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失敗（exit $LASTEXITCODE）。記憶體不足時可改用：cargo build --release -j 4，再加 -NoBuild 重新打包" }
}
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，請先建置" }

# 確認 exe 版本與 Cargo.toml 一致（避免打包到舊檔）
$exeVer = (& $Exe --version).Trim()
if ($exeVer -ne "ssh-mcp $Version") { throw "exe 版本（$exeVer）與 Cargo.toml（$Version）不一致，請重新建置" }

# ---- 準備輸出目錄 ----
if (Test-Path $Stage) { Remove-Item $Stage -Recurse -Force }
New-Item -ItemType Directory -Force $Dist | Out-Null
$McpbStage = Join-Path $Stage 'mcpb'
$ZipName   = "ssh-mcp-$Version-win-x64"
$ZipStage  = Join-Path $Stage $ZipName
New-Item -ItemType Directory -Force (Join-Path $McpbStage 'server'), $ZipStage | Out-Null

$McpbOut = Join-Path $Dist "ssh-mcp-$Version.mcpb"
$ZipOut  = Join-Path $Dist "$ZipName.zip"
Remove-Item $McpbOut, $ZipOut -ErrorAction SilentlyContinue

# ---- .mcpb ----
Step '準備 .mcpb 內容'
$manifest = Get-Content (Join-Path $PkgDir 'manifest.json') -Raw -Encoding UTF8
# 只替換頂層 "version"（"manifest_version" 前面不是引號開頭，不會被比對到）
$manifest = [regex]::new('("version"\s*:\s*")[^"]*(")').Replace($manifest, "`${1}$Version`${2}", 1)
[IO.File]::WriteAllText((Join-Path $McpbStage 'manifest.json'), $manifest, $Utf8NoBom)
Copy-Item $Exe (Join-Path $McpbStage 'server\ssh-mcp.exe')
Copy-Item (Join-Path $Root 'config.example.toml') $McpbStage

Add-Type -AssemblyName System.IO.Compression.FileSystem
$npx = Get-Command npx -ErrorAction SilentlyContinue
if ($npx) {
    Step "以 $McpbCli 驗證並打包"
    & npx -y $McpbCli validate (Join-Path $McpbStage 'manifest.json')
    if ($LASTEXITCODE -ne 0) { throw 'manifest.json 驗證失敗' }
    & npx -y $McpbCli pack $McpbStage $McpbOut
    if ($LASTEXITCODE -ne 0) { throw 'mcpb pack 失敗' }
} else {
    Step '找不到 npx，改用 .NET ZipFile 直接產生 .mcpb（未經官方工具驗證）'
    [IO.Compression.ZipFile]::CreateFromDirectory($McpbStage, $McpbOut, [IO.Compression.CompressionLevel]::Optimal, $false)
}

# ---- zip（手動安裝）----
Step '準備 zip 內容'
Copy-Item $Exe $ZipStage
Copy-Item (Join-Path $Root 'config.example.toml') $ZipStage
Copy-Item (Join-Path $PkgDir 'install.ps1') $ZipStage
Copy-Item (Join-Path $PkgDir 'claude_desktop_config.example.json') $ZipStage
Copy-Item (Join-Path $Root 'INSTALL.md') $ZipStage
[IO.Compression.ZipFile]::CreateFromDirectory($ZipStage, $ZipOut, [IO.Compression.CompressionLevel]::Optimal, $true)

# ---- SHA256 ----
Step 'SHA256SUMS.txt'
$sums = foreach ($f in @($Exe, $McpbOut, $ZipOut)) {
    $h = (Get-FileHash $f -Algorithm SHA256).Hash.ToLower()
    "$h  $(Split-Path $f -Leaf)"
}
[IO.File]::WriteAllLines((Join-Path $Dist 'SHA256SUMS.txt'), [string[]]$sums, $Utf8NoBom)

Remove-Item $Stage -Recurse -Force

Write-Host ''
Write-Host '完成：' -ForegroundColor Green
Get-ChildItem $Dist -File | Format-Table Name, @{n='Size(KB)'; e={[math]::Round($_.Length / 1KB)}} -AutoSize
$sums | ForEach-Object { Write-Host "  $_" }
