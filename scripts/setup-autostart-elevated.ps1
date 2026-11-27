<#
.SYNOPSIS
    Registers or removes an elevated Windows logon task for Thermal Guard so
    PawnIO CPU MSR monitoring works automatically at login with no UAC prompt.

.EXAMPLE
    .\scripts\setup-autostart-elevated.ps1
    .\scripts\setup-autostart-elevated.ps1 -Remove
#>
[CmdletBinding()]
param(
    [switch]$Remove,
    [string]$ExePath = (Join-Path $PSScriptRoot "..\target\release\thermal-guard.exe")
)

$ErrorActionPreference = "Stop"
$TaskName = "ThermalGuard"

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Please run this script from an elevated (Administrator) PowerShell terminal."
    exit 1
}

if ($Remove) {
    if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Host "Removed scheduled task '$TaskName'."
    } else {
        Write-Host "Scheduled task '$TaskName' is not installed."
    }
    exit 0
}

$resolvedExe = (Resolve-Path $ExePath -ErrorAction SilentlyContinue)
if (-not $resolvedExe) {
    Write-Error "Executable not found at '$ExePath'. Run 'cargo build --release' first or pass -ExePath."
    exit 1
}

$action = New-ScheduledTaskAction -Execute $resolvedExe.Path
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$taskPrincipal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew

Register-ScheduledTask `
    -TaskName $TaskName `
    -Action $action `
    -Trigger $trigger `
    -Principal $taskPrincipal `
    -Settings $settings `
    -Force | Out-Null

Write-Host "Registered elevated logon task '$TaskName' -> $($resolvedExe.Path)"
Write-Host "You can start it immediately with: Start-ScheduledTask -TaskName '$TaskName'"
