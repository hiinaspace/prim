# First-logon setup for the Prim smoke-test VM (run once by autounattend.xml).
# Leaves Defender, SmartScreen and Smart App Control as shipped, for fidelity.
param([string]$Media = "D:")
$ErrorActionPreference = "Continue"
Start-Transcript -Path C:\prim-setup-transcript.txt -Append

# OpenSSH server (Feature on Demand; needs Windows Update reachability).
for ($i = 0; $i -lt 10; $i++) {
    $cap = Get-WindowsCapability -Online -Name "OpenSSH.Server*"
    if ($cap.State -eq "Installed") { break }
    Add-WindowsCapability -Online -Name $cap.Name
    Start-Sleep -Seconds 15
}
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd
New-ItemProperty -Path "HKLM:\SOFTWARE\OpenSSH" -Name DefaultShell -Value "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" -PropertyType String -Force
if (-not (Get-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -DisplayName "OpenSSH Server (sshd)" -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
}
# The capability's rule covers the Private profile only, and QEMU's NAT network comes
# up as Public (as on many friends' PCs; left that way so Prim's firewall prompt is real).
Set-NetFirewallRule -Name "OpenSSH-Server-In-TCP" -Profile Any -Enabled True
# The prim user is an administrator, so sshd reads administrators_authorized_keys.
$keys = "C:\ProgramData\ssh\administrators_authorized_keys"
Copy-Item "$Media\authorized_keys" $keys -Force
icacls $keys /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F"

# Never sleep or blank the screen (screenshots, long runs); no reboots under a logged-on user.
powercfg /change monitor-timeout-ac 0
powercfg /change standby-timeout-ac 0
powercfg /change hibernate-timeout-ac 0
New-Item -Path "HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU" -Force | Out-Null
New-ItemProperty -Path "HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU" -Name NoAutoRebootWithLoggedOnUsers -Value 1 -PropertyType DWord -Force

# Record what a friend's fresh machine would also have.
$info = [ordered]@{
    os = (Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber)
    smart_app_control = (Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy" -ErrorAction SilentlyContinue).VerifiedAndReputablePolicyState
    video = (Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion)
    audio = (Get-CimInstance Win32_SoundDevice | Select-Object Name)
}
$info | ConvertTo-Json -Depth 4 | Set-Content C:\prim-vm-info.json
"done $(Get-Date -Format o)" | Set-Content C:\prim-setup-done.txt
Stop-Transcript
shutdown /s /t 20 /c "prim VM base image ready"
