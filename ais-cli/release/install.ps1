# ais · AI Sessions installer for Windows (PowerShell 5+).
#
#   irm https://ais.repentandliveholy.org/install.ps1 | iex
#
# Downloads ais.exe from the latest GitHub release, checks its SHA-256 against
# the release SHA256SUMS, installs it to %LOCALAPPDATA%\ais\bin and adds that
# folder to the user PATH. ais then keeps itself up to date.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'https://github.com/repentandliveholy/ais'
$Dir = if ($env:AIS_INSTALL_DIR) { $env:AIS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'ais\bin' }
$Version = if ($env:AIS_VERSION) { $env:AIS_VERSION } else { 'latest' }

if (-not [Environment]::Is64BitOperatingSystem) { throw 'ais requires 64-bit Windows' }
$Asset = 'ais-x86_64-pc-windows-gnu.exe'
$Base = if ($Version -eq 'latest') { "$Repo/releases/latest/download" } else { "$Repo/releases/download/v$($Version.TrimStart('v'))" }

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("ais-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "Downloading $Asset..."
    Invoke-WebRequest -UseBasicParsing "$Base/$Asset" -OutFile "$Tmp\ais.exe"
    Invoke-WebRequest -UseBasicParsing "$Base/SHA256SUMS" -OutFile "$Tmp\SHA256SUMS"
    $Line = Get-Content "$Tmp\SHA256SUMS" | Where-Object { $_ -match "\s\*?$([Regex]::Escape($Asset))$" } | Select-Object -First 1
    if (-not $Line) { throw "$Asset is missing from SHA256SUMS" }
    $Expected = ($Line -split '\s+')[0].ToLower()
    $Actual = (Get-FileHash "$Tmp\ais.exe" -Algorithm SHA256).Hash.ToLower()
    if ($Expected -ne $Actual) { throw "checksum mismatch for $Asset" }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    Move-Item -Force "$Tmp\ais.exe" (Join-Path $Dir 'ais.exe')
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

$UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($UserPath -split ';') -contains $Dir)) {
    [Environment]::SetEnvironmentVariable('Path', ($UserPath.TrimEnd(';') + ";$Dir").TrimStart(';'), 'User')
    $env:Path = "$env:Path;$Dir"
    Write-Host "Added $Dir to your user PATH (open a new terminal to use it everywhere)."
}
& (Join-Path $Dir 'ais.exe') --version
Write-Host 'Run: ais'
