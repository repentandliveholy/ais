# ais · AI Sessions installer for Windows (PowerShell 5+).
#
#   irm https://ais.repentandliveholy.org/install.ps1 | iex
#
# Downloads `ais.exe` (CLI) and `ais-gui.exe` (GUI) from the latest GitHub
# release, checks SHA-256 against SHA256SUMS, installs to %LOCALAPPDATA%\ais\bin,
# adds that folder to user PATH, and creates a Start Menu shortcut for `ais` GUI.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'https://github.com/repentandliveholy/ais'
$Dir = if ($env:AIS_INSTALL_DIR) { $env:AIS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'ais\bin' }
$Version = if ($env:AIS_VERSION) { $env:AIS_VERSION } else { 'latest' }

if (-not [Environment]::Is64BitOperatingSystem) { throw 'ais requires 64-bit Windows' }
$GuiAsset = 'ais-gui-x86_64-pc-windows-msvc.exe'
$CliAsset = 'ais-x86_64-pc-windows-gnu.exe'
$Base = if ($Version -eq 'latest') { "$Repo/releases/latest/download" } else { "$Repo/releases/download/v$($Version.TrimStart('v'))" }

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("ais-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Invoke-WebRequest -UseBasicParsing "$Base/SHA256SUMS" -OutFile "$Tmp\SHA256SUMS"
    $Sums = Get-Content "$Tmp\SHA256SUMS"

    function Install-Verified($Asset, $DestName) {
        $OutFile = Join-Path $Tmp $DestName
        Write-Host "Downloading $Asset..."
        Invoke-WebRequest -UseBasicParsing "$Base/$Asset" -OutFile $OutFile
        $Line = $Sums | Where-Object { $_ -match "\s\*?$([Regex]::Escape($Asset))$" } | Select-Object -First 1
        if (-not $Line) { throw "$Asset is missing from SHA256SUMS" }
        $Expected = ($Line -split '\s+')[0].ToLower()
        $Actual = (Get-FileHash $OutFile -Algorithm SHA256).Hash.ToLower()
        if ($Expected -ne $Actual) { throw "checksum mismatch for $Asset" }
        New-Item -ItemType Directory -Force -Path $Dir | Out-Null
        Move-Item -Force $OutFile (Join-Path $Dir $DestName)
    }

    Install-Verified $CliAsset 'ais.exe'
    Install-Verified $GuiAsset 'ais-gui.exe'
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

$UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($UserPath -split ';') -contains $Dir)) {
    [Environment]::SetEnvironmentVariable('Path', ($UserPath.TrimEnd(';') + ";$Dir").TrimStart(';'), 'User')
    $env:Path = "$env:Path;$Dir"
    Write-Host "Added $Dir to your user PATH (open a new terminal to use it everywhere)."
}

try {
    $Programs = [Environment]::GetFolderPath('Programs')
    if ($Programs) {
        $Shell = New-Object -ComObject WScript.Shell
        $Shortcut = $Shell.CreateShortcut((Join-Path $Programs 'ais.lnk'))
        $GuiExe = Join-Path $Dir 'ais-gui.exe'
        $Shortcut.TargetPath = $GuiExe
        $Shortcut.IconLocation = "$GuiExe,0"
        $Shortcut.WorkingDirectory = $env:USERPROFILE
        $Shortcut.Description = 'ais · AI Sessions'
        $Shortcut.Save()
    }
} catch {}

& (Join-Path $Dir 'ais.exe') --version
Write-Host "Installed ais (CLI) and ais-gui (GUI) to $Dir"
Write-Host 'Run: ais (CLI) or ais-gui (GUI)'
