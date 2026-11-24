# Builds the program in release mode and installs it into dist\ – that folder is the installed program:
# dist\HardwareMonitor.exe, dist\program.json, and dist\data\ with your settings (never touched by a build).
# Works while the program is running: the running exe is put aside (see below) and the new one starts next time.
#
# Requirement: Rust (https://rustup.rs). Nothing pops up; the first build takes a few minutes.
$ErrorActionPreference = 'Stop'
if ($PSScriptRoot) { Set-Location $PSScriptRoot }

# The two names that differ from program to program: the package in Cargo.toml and the exe in dist\.
$package = 'hardware-monitor'
$exe = 'HardwareMonitor.exe'

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } # stop here, so that no old exe is installed after a failed build

# Where cargo builds: target\ in this repo, or the shared build folder of a .cargo\config.toml further up.
$target = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).target_directory
$release = Join-Path $target 'release'

New-Item -ItemType Directory -Force dist | Out-Null # only ever created, never deleted: dist\data\ is yours

# Windows locks a running exe: it cannot be overwritten or deleted, but it can be renamed. So the old one becomes
# "HardwareMonitor.<time>.old.exe" – it keeps running – and the new one takes its place. The program removes such
# files itself at its next start.
$installed = Join-Path dist $exe
if (Test-Path $installed) {
    try {
        Copy-Item (Join-Path $release "$package.exe") $installed -Force # the usual case: the program is not running
    } catch {
        $old = [IO.Path]::GetFileNameWithoutExtension($exe) + '.' + (Get-Date -Format 'HHmmss') + '.old.exe'
        Rename-Item $installed $old # it is running: put it aside
        Write-Host "$exe is running - the running version is now called $old, the new one starts next time."
        Copy-Item (Join-Path $release "$package.exe") $installed -Force
    }
} else {
    Copy-Item (Join-Path $release "$package.exe") $installed -Force
}
# What the Toolbox reads; written by the build script from Cargo.toml.
Copy-Item (Join-Path $release "$package.program.json") 'dist\program.json' -Force
Copy-Item 'README.md' 'dist\README.md' -Force

# ---------- the bridge: the one part with admin rights, C# (Toolbox\DESIGN.md 10) ----------
# Compiled with the csc.exe that is part of Windows, against LibreHardwareMonitor in the version of lhm.json –
# downloaded once into the build folder and checked by its SHA-256. "Set up" copies dist\LhmBridge.exe and that
# same LibreHardwareMonitor into C:\Program Files\WindowsToolbox\hardware_monitor\.
$lhm = Get-Content 'lhm.json' -Raw | ConvertFrom-Json
$lhmDir = Join-Path $target "lhm-$($lhm.version)"
if (-not (Test-Path (Join-Path $lhmDir 'LibreHardwareMonitorLib.dll'))) {
    $zip = "$lhmDir.zip"
    Write-Host "Downloading LibreHardwareMonitor $($lhm.version) for the bridge ..."
    Invoke-WebRequest $lhm.url -OutFile $zip -UseBasicParsing
    # .NET instead of Get-FileHash/Expand-Archive: their modules are missing when PowerShell 7 started this one.
    $stream = [IO.File]::OpenRead($zip)
    try { $hash = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($stream)) -replace '-', '' }
    finally { $stream.Dispose() }
    if ($hash -ne $lhm.sha256) { Remove-Item $zip; throw "The checksum of $($lhm.url) does not match lhm.json." }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    if (Test-Path $lhmDir) { Remove-Item $lhmDir -Recurse -Force }
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $lhmDir)
    Remove-Item $zip
}
$framework = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319'
$bridge = Join-Path $release 'LhmBridge.exe'
& (Join-Path $framework 'csc.exe') /nologo /target:winexe /optimize+ "/out:$bridge" '/win32icon:assets\icon.ico' `
    "/r:$(Join-Path $lhmDir 'LibreHardwareMonitorLib.dll')" "/r:$(Join-Path $framework 'System.Web.Extensions.dll')" `
    "/r:$(Join-Path $framework 'Microsoft.CSharp.dll')" "/r:$(Join-Path $framework 'System.Core.dll')" 'bridge\LhmBridge.cs'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Copy-Item $bridge 'dist\LhmBridge.exe' -Force
Write-Host "Done: dist\$exe, dist\LhmBridge.exe"
