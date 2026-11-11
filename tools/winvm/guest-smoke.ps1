# Runs inside the Windows VM (uploaded by tools/winvm.py smoke). Unzips a Prim
# package the way a friend would get it (downloaded, so Mark-of-the-Web), then:
#   headless: prim.exe --headless over ssh (DLL loading, Rust, identity.key)
#   desktop:  Desktop.bat in the logged-on desktop session (rendering path)
# A screenshot is taken by the host between phases. Writes C:\prim\out\*.
param(
    [string]$Zip = "C:\prim\package.zip",
    [string]$Phase = "unpack",   # unpack | headless | desktop-start | doubleclick-start | collect
    [int]$Seconds = 40
)
$ErrorActionPreference = "Continue"
$root = "C:\prim"; $pkg = "$root\pkg"; $out = "$root\out"
New-Item -ItemType Directory -Force $out | Out-Null

function Write-Json($name, $obj) { $obj | ConvertTo-Json -Depth 6 | Set-Content "$out\$name" }
function Find-Exe { Get-ChildItem -Path $pkg -Recurse -Filter prim.exe | Select-Object -First 1 }

switch ($Phase) {
  "unpack" {
    # Browsers and Discord mark downloads ZoneId=3; Explorer copies that mark to every extracted file.
    Set-Content -Path $Zip -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3"
    Remove-Item -Recurse -Force $pkg -ErrorAction SilentlyContinue
    Expand-Archive -Path $Zip -DestinationPath $pkg -Force
    Get-ChildItem -Path $pkg -Recurse -File | ForEach-Object {
        Set-Content -Path $_.FullName -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3"
    }
    $exe = Find-Exe
    Write-Json "unpack.json" ([ordered]@{
        files = (Get-ChildItem -Path $pkg -Recurse -File).Count
        prim_exe = $exe.FullName
        os = (Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber)
        smart_app_control = (Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy" -ErrorAction SilentlyContinue).VerifiedAndReputablePolicyState
        video = (Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion, Status)
        vulkan_icds = (Get-ItemProperty "HKLM:\SOFTWARE\Khronos\Vulkan\Drivers" -ErrorAction SilentlyContinue | Out-String).Trim()
        audio = (Get-CimInstance Win32_SoundDevice | Select-Object Name, Status)
    })
  }
  "headless" {
    $exe = Find-Exe
    $p = Start-Process -FilePath $exe.FullName -WorkingDirectory $exe.DirectoryName -PassThru `
        -ArgumentList "--headless", "--xr-mode", "off", "--", "--desktop" `
        -RedirectStandardOutput "$out\headless-stdout.txt" -RedirectStandardError "$out\headless-stderr.txt"
    $exited = $p.WaitForExit($Seconds * 1000)
    if (-not $exited) { Stop-Process -Id $p.Id -Force }
    Write-Json "headless.json" ([ordered]@{ ran_seconds = $Seconds; exited_early = $exited; exit_code = $(if ($exited) { $p.ExitCode } else { $null }) })
  }
  "desktop-start" {
    # Run Desktop.bat as a friend would, but via cmd (CreateProcess) so SmartScreen
    # can't hold it; the interactive task makes it a real desktop-session window.
    $exe = Find-Exe
    "@echo off`r`ncd /d `"$($exe.DirectoryName)`"`r`ncall Desktop.bat > $out\desktop-stdout.txt 2>&1" | Set-Content "$root\run-desktop.cmd" -Encoding ascii
    schtasks /Create /TN PrimSmoke /TR "$root\run-desktop.cmd" /SC ONCE /ST 23:59 /RU prim /IT /F | Out-Null
    schtasks /Run /TN PrimSmoke
  }
  "doubleclick-start" {
    # ShellExecute Desktop.bat in the desktop session, the double-click path that
    # SmartScreen intercepts for downloaded files. The host screenshots the result.
    $exe = Find-Exe
    "Start-Process -FilePath `"$($exe.DirectoryName)\Desktop.bat`"" | Set-Content "$root\doubleclick.ps1"
    schtasks /Create /TN PrimClick /TR "powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File $root\doubleclick.ps1" /SC ONCE /ST 23:59 /RU prim /IT /F | Out-Null
    schtasks /Run /TN PrimClick
  }
  "collect" {
    $procs = Get-Process -Name prim -ErrorAction SilentlyContinue | Select-Object Id, StartTime, Responding, MainWindowTitle, @{n="WorkingSetMB";e={[int]($_.WorkingSet64/1MB)}}
    Write-Json "processes.json" @($procs)
    Get-Process -Name prim -ErrorAction SilentlyContinue | Stop-Process -Force
    Get-Process -Name smartscreen -ErrorAction SilentlyContinue | Select-Object Id | ConvertTo-Json | Set-Content "$out\smartscreen-procs.json"
    $appdata = "C:\Users\prim\AppData\Roaming"
    $udirs = @("$appdata\prim", "$appdata\Godot\app_userdata") | Where-Object { Test-Path $_ }
    foreach ($d in $udirs) {
        Get-ChildItem -Path $d -Recurse -File | Where-Object { $_.Length -lt 5MB } | ForEach-Object {
            $rel = $_.FullName.Substring($appdata.Length + 1)
            $dest = Join-Path "$out\appdata" $rel
            New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
            Copy-Item $_.FullName $dest
        }
    }
    Get-WinEvent -FilterHashtable @{LogName="Application"; Level=1,2,3; StartTime=(Get-Date).AddHours(-1)} -ErrorAction SilentlyContinue |
        Select-Object TimeCreated, ProviderName, Id, LevelDisplayName, Message | ConvertTo-Json -Depth 3 | Set-Content "$out\app-events.json"
    Get-ChildItem "C:\ProgramData\Microsoft\Windows\WER\ReportArchive", "C:\Users\prim\AppData\Local\CrashDumps" -ErrorAction SilentlyContinue |
        Select-Object FullName, LastWriteTime | ConvertTo-Json | Set-Content "$out\crash-reports.json"
    Compress-Archive -Path "$out\*" -DestinationPath "$root\out.zip" -Force
  }
}
