# Install the release build for the current user: no admin, no installer, no
# registry writes beyond the optional Run entry.
#
# Defender is scanned, never disabled, excluded or asked to look away — that
# order is the point of the script. Run it and the scan is the first thing that
# happens, so the binary is checked before it is put anywhere a shell resolves
# it from.
#
#   .\install.ps1              # install and start it
#   .\install.ps1 -NoStart     # install only
#   .\install.ps1 -Autostart   # install and add a Run entry
param(
    [switch]$NoStart,
    [switch]$Autostart
)

$ErrorActionPreference = 'Stop'

$src = Join-Path $PSScriptRoot 'target\release\arbotray.exe'
if (-not (Test-Path $src)) {
    throw "$src is missing - run: cargo build --release"
}

$dir = Join-Path $env:LOCALAPPDATA 'Programs\ArboTray'
$dst = Join-Path $dir 'arbotray.exe'

# 1. What Defender is doing right now. Not fatal if it is absent: an unsigned
#    binary is only a question for the *active* antivirus, and a machine
#    running a different one answers it elsewhere.
$svc = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
if (Test-Path $svc) {
    Get-MpComputerStatus |
        Format-List RealTimeProtectionEnabled, AntivirusEnabled, AMServiceEnabled
    & $svc -Scan -ScanType 3 -File $src -DisableRemediation
    if ($LASTEXITCODE -ne 0) {
        throw "Defender refused to scan ($LASTEXITCODE) - see $env:TEMP\MpCmdRun.log"
    }
} else {
    Write-Warning 'Windows Defender is not present; no scan was run.'
}

# 2. Replace the binary. Moved aside rather than deleted, so an install that
#    turns out to be worse than what it replaced can be undone by hand.
New-Item -ItemType Directory -Force $dir | Out-Null
Get-Process arbotray -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 250
if (Test-Path $dst) { Move-Item -Force $dst "$dst.bak-prev" }
Copy-Item $src $dst
Write-Host "installed $dst"

# 3. A Start Menu entry, so it is findable without a terminal.
$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'ArboTray.lnk'
$shell = New-Object -ComObject WScript.Shell
$s = $shell.CreateShortcut($lnk)
$s.TargetPath = $dst
$s.WorkingDirectory = $dir
$s.Save()

if ($Autostart) {
    Set-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' ArboTray "`"$dst`""
}

if (-not $NoStart) {
    Start-Process $dst
    Write-Host 'running - right-click its tray icon to open the dashboard or exit'
}
