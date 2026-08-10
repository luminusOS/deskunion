<#
.SYNOPSIS
    Register DeskUnion with Windows Defender Firewall so it never asks
    for network permission again.

.DESCRIPTION
    DeskUnion in client mode only dials out to the server; it opens no
    listening port. Windows still shows the "allow network access"
    prompt the first time a program touches a socket, and a dismissed
    prompt leaves behind a *block* rule. This script replaces any such
    leftover with explicit program-scoped rules.

    In server mode the machine listens on UDP 4242 (or -Port), so an
    inbound allow rule is required as well — pass -Server for that.

    Run from an elevated PowerShell:
        powershell -ExecutionPolicy Bypass -File windows-firewall.ps1

.PARAMETER ExePath
    Path to deskunion.exe. Defaults to bin\deskunion.exe next to this
    script (the layout of the distributed zip).

.PARAMETER Server
    Also open the inbound UDP listen port (server mode only).

.PARAMETER Port
    Listen port for -Server. Defaults to 4242.

.PARAMETER Remove
    Delete the rules instead of creating them.
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [switch]$Server,
    [int]$Port = 4242,
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Run this script from an elevated PowerShell (Run as administrator)."
}

if (-not $ExePath) {
    $ExePath = Join-Path (Split-Path -Parent $PSCommandPath) 'bin\deskunion.exe'
}
$ExePath = (Resolve-Path -LiteralPath $ExePath).Path

$outboundRule = 'DeskUnion (outbound)'
$inboundRule  = "DeskUnion (inbound UDP $Port)"

# a dismissed prompt leaves a block rule behind: clear every rule for
# this program before adding ours, in both directions
foreach ($name in @($outboundRule, $inboundRule)) {
    Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue |
        Remove-NetFirewallRule -ErrorAction SilentlyContinue
}
Get-NetFirewallApplicationFilter -Program $ExePath -ErrorAction SilentlyContinue |
    Get-NetFirewallRule -ErrorAction SilentlyContinue |
    Where-Object { $_.Action -eq 'Block' } |
    Remove-NetFirewallRule -ErrorAction SilentlyContinue

if ($Remove) {
    Write-Host "Removed the DeskUnion firewall rules for $ExePath."
    return
}

New-NetFirewallRule -DisplayName $outboundRule -Direction Outbound -Action Allow `
    -Program $ExePath -Profile Any -Protocol UDP | Out-Null
Write-Host "Allowed outbound UDP for $ExePath."

if ($Server) {
    New-NetFirewallRule -DisplayName $inboundRule -Direction Inbound -Action Allow `
        -Program $ExePath -Profile Domain,Private -Protocol UDP -LocalPort $Port | Out-Null
    Write-Host "Allowed inbound UDP $Port for $ExePath (private/domain networks)."
} else {
    Write-Host "Client mode: no inbound rule created (DeskUnion opens no port)."
}
