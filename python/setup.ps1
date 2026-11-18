<#
"Set up" on the Hardware Monitor's page runs this in a window of its own. Everything it does is checked first, so
running it again only does what is missing:

  1. Python 3.14 (installed via winget if it is missing) – copied once into runtime\ next to HardwareMonitor.exe,
     so the monitor uses only what is in this folder
  2. the packages from requirements.txt (fixed versions)
  3. runtime\HardwareMonitorSensors.exe: pythonw.exe with its own name and icon (make_exe.py) – in Task Manager it
     is "Hardware Monitor – sensors" instead of "Python"
  4. LibreHardwareMonitor into lhm\ (pinned version, checksum)
  5. with admin rights, one prompt for both: PawnIO (the driver LibreHardwareMonitor reads CPU and mainboard
     sensors through) and the scheduled task "HardwareMonitor", which starts the sensors with admin rights
     without asking. The task has no trigger: whether the monitor starts with Windows is the switch in the Toolbox.

Where: the folder with HardwareMonitor.exe – dist\ in the repo, the unpacked folder of a release.
#>
# -Admin: step 5 (started by the script itself). -NoAdmin: steps 1–4 only (for testing).
param([switch]$Admin, [switch]$NoAdmin)

$ErrorActionPreference = 'Stop'
$PythonVersion = '3.14'
$TaskName = 'HardwareMonitor'
$Lhm = @{
    Version = '0.9.6'
    Url = 'https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/download/v0.9.6/LibreHardwareMonitor.zip'
    Sha256 = '086D9F1B5A99E643EDC2CFAAAC16051685B551E4C5AC0B32A57C58C0E529C001'
    CheckFile = 'LibreHardwareMonitorLib.dll'
}

$PythonDir = $PSScriptRoot
$parent = Split-Path -Parent $PythonDir
$Home_ = if (Test-Path (Join-Path $parent 'program.json')) { $parent } else { Join-Path $parent 'dist' }
$Runtime = Join-Path $Home_ 'runtime'
$RuntimePy = Join-Path $Runtime 'python.exe'
$Sensors = Join-Path $Runtime 'HardwareMonitorSensors.exe'
$Script = Join-Path $PythonDir 'hardware_monitor.py'
$LhmDir = Join-Path $Home_ 'lhm'

function Step($text) { Write-Host ""; Write-Host "== $text" -ForegroundColor Cyan }
function Ok($text)   { Write-Host "   [OK] $text" -ForegroundColor Green }
function Info($text) { Write-Host "   $text" }

function Test-TaskFits {
    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    $task -and $task.Actions[0].Execute -eq $Sensors -and $task.Actions[0].Arguments -eq "`"$Script`"" -and
        $task.Principal.RunLevel -eq 'Highest' -and -not $task.Triggers
}

# ---------- 5. with admin rights (started by the part below, once) ----------
if ($Admin) {
    try {
        if (-not (Get-Service PawnIO -ErrorAction SilentlyContinue)) {
            Step 'PawnIO'
            winget install --id namazso.PawnIO --exact --accept-package-agreements --accept-source-agreements | Out-Host
            if ($LASTEXITCODE -ne 0) { throw "winget reports code $LASTEXITCODE" }
            Ok 'installed'
        }
        if (-not (Test-TaskFits)) {
            Step "Task $TaskName"
            $user = "$env:USERDOMAIN\$env:USERNAME"
            $action = New-ScheduledTaskAction -Execute $Sensors -Argument "`"$Script`"" -WorkingDirectory $PythonDir
            $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
            $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
                -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
            # Deleted first: registering over an existing task with -Force would keep its triggers.
            Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
            Register-ScheduledTask -TaskName $TaskName -Action $action -Principal $principal -Settings $settings | Out-Null
            Ok 'set up'
        }
        exit 0
    } catch {
        Write-Host "   [!] $_" -ForegroundColor Yellow
        Read-Host "`nPress Enter to close" | Out-Null
        exit 1
    }
}

try {
    # ---------- 1. Python and runtime\ ----------
    Step "Python $PythonVersion in runtime\"
    if (Test-Path $RuntimePy) {
        Ok 'runtime\ is already there'
    } else {
        $python = $null
        foreach ($hive in 'HKCU:', 'HKLM:') {
            $exe = (Get-ItemProperty "$hive\Software\Python\PythonCore\$PythonVersion\InstallPath" `
                        -ErrorAction SilentlyContinue).ExecutablePath
            if ($exe -and (Test-Path $exe)) { $python = $exe; break }
        }
        if (-not $python) {
            Info "Python $PythonVersion not found – installing it via winget …"
            winget install --id "Python.Python.$PythonVersion" --exact --scope user --accept-package-agreements --accept-source-agreements
            foreach ($hive in 'HKCU:', 'HKLM:') {
                $exe = (Get-ItemProperty "$hive\Software\Python\PythonCore\$PythonVersion\InstallPath" `
                            -ErrorAction SilentlyContinue).ExecutablePath
                if ($exe -and (Test-Path $exe)) { $python = $exe; break }
            }
            if (-not $python) { throw "Python $PythonVersion could not be installed. Please install it from python.org and press Set up again." }
        }
        $base = Split-Path -Parent $python
        Info "Copying $base …"
        # Without docs, tests, developer files and installed packages (those come fresh in a moment)
        robocopy $base $Runtime /E /NFL /NDL /NJH /NJS /NP `
            /XD (Join-Path $base 'Doc') (Join-Path $base 'include') (Join-Path $base 'libs') `
                (Join-Path $base 'Scripts') (Join-Path $base 'Tools') (Join-Path $base 'Lib\site-packages') `
                (Join-Path $base 'Lib\test') __pycache__ `
            /XF *.pyc | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "Copying failed (robocopy code $LASTEXITCODE)" }
        New-Item -ItemType Directory -Force (Join-Path $Runtime 'Lib\site-packages') | Out-Null
        # Only what is in runtime\: fixed search paths (._pth), no packages from the user folder (sitecustomize.py)
        $dll = Get-ChildItem $Runtime -Filter 'python3*.dll' | Where-Object Name -ne 'python3.dll' | Select-Object -First 1
        "$($dll.BaseName).zip`r`nDLLs`r`nLib`r`nimport site`r`n" |
            Set-Content -Encoding ASCII (Join-Path $Runtime "$($dll.BaseName)._pth")
        "# Created by setup.ps1: don't use packages from the user folder (%APPDATA%\Python)`r`n" +
        "import site, sys`r`n" +
        "if site.getusersitepackages() in sys.path:`r`n" +
        "    sys.path.remove(site.getusersitepackages())`r`n" |
            Set-Content -Encoding ASCII (Join-Path $Runtime 'Lib\sitecustomize.py')
        Info 'Installing pip …'
        $ErrorActionPreference = 'Continue'
        & $RuntimePy -m ensurepip --default-pip 2>&1 | Out-Null
        $ErrorActionPreference = 'Stop'
        if ($LASTEXITCODE -ne 0) { throw 'pip could not be set up' }
        Ok 'runtime\ created'
    }

    # ---------- 2. packages ----------
    Step 'Packages from requirements.txt'
    & $RuntimePy -c 'import ssl'
    if ($LASTEXITCODE -ne 0) { throw 'runtime\ is incomplete (ssl is missing) – delete the folder runtime\ and press Set up again' }
    & $RuntimePy -m pip install --disable-pip-version-check --no-warn-script-location --quiet `
        -r (Join-Path $PythonDir 'requirements.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Packages could not be installed' }
    & $RuntimePy -c 'import tkinter, pystray, PIL, clr'
    if ($LASTEXITCODE -ne 0) { throw 'runtime\ is incomplete – delete the folder runtime\ and press Set up again' }
    Ok 'complete'

    # ---------- 3. the sensors' exe ----------
    Step 'runtime\HardwareMonitorSensors.exe'
    $message = & $RuntimePy -B (Join-Path $PythonDir 'make_exe.py') $Sensors 'Hardware Monitor – sensors' (Join-Path $PythonDir 'program.ico')
    if ($LASTEXITCODE -eq 2) { throw "$message – stop the Hardware Monitor first" }
    if ($LASTEXITCODE -ne 0) { throw $message }
    Ok $message

    # ---------- 4. LibreHardwareMonitor ----------
    Step "LibreHardwareMonitor $($Lhm.Version)"
    if (Test-Path (Join-Path $LhmDir $Lhm.CheckFile)) {
        Ok 'already there'
    } else {
        $zip = Join-Path ([IO.Path]::GetTempPath()) "hardware_monitor-lhm-$($Lhm.Version).zip"
        Info "Downloading $($Lhm.Url) …"
        Invoke-WebRequest $Lhm.Url -OutFile $zip -UseBasicParsing
        # .NET instead of Get-FileHash and Expand-Archive: those live in modules that are missing when PowerShell 7
        # started this window (its module path is inherited).
        $stream = [IO.File]::OpenRead($zip)
        try { $hash = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($stream)) -replace '-', '' }
        finally { $stream.Dispose() }
        if ($hash -ne $Lhm.Sha256) {
            Remove-Item $zip
            throw 'Checksum of the downloaded file doesn''t match – download aborted.'
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        New-Item -ItemType Directory -Force $LhmDir | Out-Null
        [IO.Compression.ZipFile]::ExtractToDirectory($zip, $LhmDir)
        Remove-Item $zip
        # No "downloaded from the internet" mark on the files, otherwise .NET won't load the DLLs
        Get-ChildItem $LhmDir -Recurse -File | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Stream Zone.Identifier -ErrorAction SilentlyContinue }
        Ok "extracted to $LhmDir"
    }

    # ---------- 5. admin rights ----------
    Step 'PawnIO and task (admin rights)'
    if ($NoAdmin) {
        Info 'skipped (-NoAdmin)'
    } elseif ((Get-Service PawnIO -ErrorAction SilentlyContinue) -and (Test-TaskFits)) {
        Ok 'both are there'
    } else {
        Info 'Windows asks for admin rights once …'
        $p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -ArgumentList @(
            '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Admin')
        if ($p.ExitCode -ne 0) { throw 'not everything worked (see the window that asked for admin rights)' }
        Ok 'done'
    }

    Write-Host ""
    Write-Host 'Set up. This window closes in a few seconds.' -ForegroundColor Green
    Start-Sleep 4
} catch {
    Write-Host ""
    Write-Host "   [!] $_" -ForegroundColor Yellow
    Read-Host "`nPress Enter to close" | Out-Null
    exit 1
}
