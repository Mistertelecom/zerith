# Builds Zerith (CLI + Tauri 2 desktop) and runs it from a folder that does NOT
# inherit a Low integrity label. A program that runs from a labeled folder (for
# example one a sandbox tool marked) inherits that label and Windows then
# refuses it permission to create folders. `zerith doctor` explains the same thing.
#
# Usage: .\scripts\run-clean.ps1            build, copy, open the window
#        .\scripts\run-clean.ps1 -NoRun     build and copy only
param([switch]$NoRun, [string]$Dest = "$env:LOCALAPPDATA\Programs\Zerith")

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    Push-Location "$root\crates\zerith-desktop\frontend"
    try {
        npm run build
        if ($LASTEXITCODE -ne 0) { throw "npm run build falhou" }
    } finally { Pop-Location }

    cargo build -p zerith-cli -p zerith-desktop
    if ($LASTEXITCODE -ne 0) { throw "cargo build falhou" }
    New-Item -ItemType Directory -Force $Dest | Out-Null
    # Stop a running copy first: Windows will not overwrite a running exe.
    Get-Process zerith-desktop, zerith -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$Dest*" } | Stop-Process -Force
    Copy-Item "$root\target\debug\zerith.exe", "$root\target\debug\zerith-desktop.exe" $Dest -Force

    # Ready-made skills and the programs they rely on (agent-desktop for
    # computer use) travel next to the executables. The app adds the skills,
    # disabled, on its first start; the shell tool puts bundled-tools on PATH
    # for its own child processes only.
    foreach ($folder in "bundled-skills", "bundled-tools") {
        if (Test-Path "$root\$folder") {
            Remove-Item -Recurse -Force "$Dest\$folder" -ErrorAction SilentlyContinue
            Copy-Item "$root\$folder" "$Dest\$folder" -Recurse -Force
        }
    }
    "copiado para $Dest"
    if (-not $NoRun) { Start-Process "$Dest\zerith-desktop.exe" -WorkingDirectory $root }
} finally { Pop-Location }
