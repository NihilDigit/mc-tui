# shulker installer for Windows.
#
# What it does:
#   1. Detects PROCESSOR_ARCHITECTURE → release-asset triple.
#   2. Asks GitHub for the latest tag.
#   3. Downloads the matching .zip, extracts it, drops `shulker.exe` into
#      $env:SHULKER_INSTALL_DIR (default: $env:LOCALAPPDATA\shulker).
#   4. Tells you to add the dir to PATH if it isn't already there.
#
# Env vars:
#   $env:SHULKER_INSTALL_DIR  install dir (default: $env:LOCALAPPDATA\shulker)
#   $env:SHULKER_VERSION      pin a release tag (default: latest)
#   $env:MC_TUI_INSTALL_DIR / $env:MC_TUI_VERSION are accepted as fallbacks
#   for pre-rename users — new docs should use SHULKER_*.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/NihilDigit/shulker/main/scripts/install.ps1 | iex
#
# To pin a version:
#   $env:SHULKER_VERSION = "v1.0.0"; irm ... | iex

$ErrorActionPreference = "Stop"

$Repo = "NihilDigit/shulker"
$InstallDir =
    if ($env:SHULKER_INSTALL_DIR) { $env:SHULKER_INSTALL_DIR }
    elseif ($env:MC_TUI_INSTALL_DIR) { $env:MC_TUI_INSTALL_DIR }
    else { "$env:LOCALAPPDATA\shulker" }

# 1. Detect arch
$triple = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64-pc-windows-msvc" }
    "ARM64" { "aarch64-pc-windows-msvc" }
    default {
        Write-Host "✗ unsupported arch: $env:PROCESSOR_ARCHITECTURE" -ForegroundColor Red
        Write-Host "  Supported: AMD64 (x86_64) and ARM64."
        exit 1
    }
}

# 2. Latest tag via GitHub API (or pinned version)
Write-Host "→ resolving latest shulker release for $triple..."
$pinned = if ($env:SHULKER_VERSION) { $env:SHULKER_VERSION } elseif ($env:MC_TUI_VERSION) { $env:MC_TUI_VERSION } else { $null }
if ($pinned) {
    $tag = $pinned
} else {
    try {
        # User-Agent is required by GH API.
        $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "shulker-install" }
        $tag = $rel.tag_name
    } catch {
        Write-Host "✗ failed to resolve latest tag: $_" -ForegroundColor Red
        Write-Host "  Set `$env:SHULKER_VERSION = 'vX.Y.Z'` to override."
        exit 1
    }
}
Write-Host "→ tag: $tag"

# 3. Download + extract
$asset = "shulker-$tag-$triple.zip"
$url = "https://github.com/$Repo/releases/download/$tag/$asset"
$tmp = Join-Path $env:TEMP "shulker-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "→ downloading $url"
    Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $asset) -UseBasicParsing
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp

    $extracted = Join-Path $tmp "shulker-$tag-$triple"
    if (-not (Test-Path (Join-Path $extracted "shulker.exe"))) {
        Write-Host "✗ archive is missing shulker.exe at $extracted" -ForegroundColor Red
        exit 1
    }

    # 4. Install
    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir | Out-Null
    }
    Copy-Item -Path (Join-Path $extracted "shulker.exe") -Destination $InstallDir -Force
    Write-Host "✓ installed: $InstallDir\shulker.exe"
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# 5. PATH check
$paths = $env:PATH -split ";"
if ($paths -notcontains $InstallDir) {
    Write-Host ""
    Write-Host "⚠ $InstallDir is not in your PATH."
    Write-Host "  Add it for this user (one-time):"
    Write-Host "    [Environment]::SetEnvironmentVariable('PATH', '$InstallDir;' + [Environment]::GetEnvironmentVariable('PATH', 'User'), 'User')"
    Write-Host "  Then restart your shell."
}

Write-Host ""
Write-Host "Run:"
Write-Host "  shulker --server-dir 'C:\path\to\your\server'"
Write-Host "  shulker new 'C:\path\to\fresh\server-dir'   # scaffold a new Paper/Purpur server"
