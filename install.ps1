# komari-agent-rs Windows Installer
# Downloads binary from GitHub Releases, verifies SHA256, creates config,
# and registers a Scheduled Task for continuous execution.
#
# Usage:
#   .\install.ps1 -Token "mytoken" -Endpoint "https://komari.example.com"
#   .\install.ps1 -Token "mytoken" -Endpoint "..." -Version "v0.2.0"
#   .\install.ps1 -Token "mytoken" -Endpoint "..." -GitHubProxy "https://ghproxy.com"

param(
    [string]$Token,
    [string]$Endpoint,
    [string]$Version = "",
    [string]$InstallDir = "$env:ProgramData\komari-agent",
    [string]$GitHubProxy = ""
)

$ErrorActionPreference = "Stop"
$Repo       = "DeliciousBuding/komari-agent-rs"
$BinaryName = "komari-agent-rs.exe"
$ConfigName = "config.json"
$TaskName   = "komari-agent-rs"

# ── Logging ──────────────────────────────────────────────────────────
function info  { Write-Host "  $args" -ForegroundColor Cyan }
function ok    { Write-Host "  [OK] $args" -ForegroundColor Green }
function warn  { Write-Host "  [WARN] $args" -ForegroundColor Yellow }
function fatal { Write-Host "[FATAL] $args" -ForegroundColor Red; exit 1 }

# ── Admin check ──────────────────────────────────────────────────────
$isAdmin = [Security.Principal.WindowsPrincipal]::new(
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
if (-not $isAdmin) { fatal "Please run this script as Administrator." }

# ── Architecture detection ───────────────────────────────────────────
$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'amd64' }
    'ARM64' { 'arm64' }
    default { fatal "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}

# ── Determine version ────────────────────────────────────────────────
if ($Version) {
    $tag = $Version
    info "Using specified version: $tag"
} else {
    info "Fetching latest release from GitHub API..."
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing
        $tag = $release.tag_name
        ok "Latest: $tag"
    } catch { fatal "Failed to fetch latest version: $_" }
}

# ── Construct URLs ───────────────────────────────────────────────────
$asset    = "komari-agent-rs-windows-$arch.exe"
$base     = if ($GitHubProxy) { "$GitHubProxy/https://github.com" } else { "https://github.com" }
$dlUrl    = "$base/$Repo/releases/download/$tag/$asset"
$sha256Url = "$dlUrl.sha256"

# ── Prepare install directory ────────────────────────────────────────
$null = New-Item -ItemType Directory -Path $InstallDir -Force
$binPath    = Join-Path $InstallDir $BinaryName
$configPath = Join-Path $InstallDir $ConfigName

# ── Remove existing task ─────────────────────────────────────────────
$existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existing) {
    info "Removing existing scheduled task '$TaskName'..."
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
}

# ── Download binary ──────────────────────────────────────────────────
info "Downloading: $dlUrl"
try { Invoke-WebRequest -Uri $dlUrl -OutFile $binPath -UseBasicParsing }
catch { fatal "Download failed: $_" }
$binSize = (Get-Item $binPath).Length
ok "Downloaded $asset ($('{0:N0}' -f $binSize) bytes)"

# ── SHA256 verification ──────────────────────────────────────────────
info "Downloading SHA256 checksum..."
try {
    $sha256Resp = Invoke-WebRequest -Uri $sha256Url -UseBasicParsing
    $expectedHash = ($sha256Resp.Content -split '\s+')[0].Trim().ToLower()
} catch {
    warn "Could not download SHA256 from $sha256Url (skipping verification)"
    $expectedHash = $null
}
if ($expectedHash) {
    $actualHash = (Get-FileHash -Path $binPath -Algorithm SHA256).Hash.ToLower()
    if ($actualHash -eq $expectedHash) {
        ok "SHA256 verified: $actualHash"
    } else {
        Remove-Item $binPath -Force
        fatal "SHA256 mismatch! Expected: $expectedHash  Got: $actualHash"
    }
}

# ── Create config template ───────────────────────────────────────────
$config = @{
    endpoint = if ($Endpoint) { $Endpoint } else { "https://your-komari-server" }
    token    = if ($Token)    { $Token }    else { "your-token-here" }
}
$config | ConvertTo-Json -Depth 3 | Set-Content -Path $configPath -Encoding UTF8
ok "Config: $configPath"

# ── Build agent arguments ────────────────────────────────────────────
$agentArgs = @()
if ($Token)    { $agentArgs += '--token';    $agentArgs += $Token }
if ($Endpoint) { $agentArgs += '--endpoint'; $agentArgs += $Endpoint }
$agentArgs += '--config'; $agentArgs += $configPath
$argString = $agentArgs -join ' '

# ── Register Scheduled Task ──────────────────────────────────────────
info "Registering Scheduled Task '$TaskName'..."
$action  = New-ScheduledTaskAction -Execute $binPath -Argument $argString
$trigger = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
    -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -MultipleInstances IgnoreNew
$principal = New-ScheduledTaskPrincipal -UserID "NT AUTHORITY\SYSTEM" -LogonType ServiceAccount -RunLevel Highest

Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Settings $settings -Principal $principal -Force | Out-Null

# ── Start the task ───────────────────────────────────────────────────
info "Starting Scheduled Task..."
Start-ScheduledTask -TaskName $TaskName
ok "Task '$TaskName' started"

# ── Summary ──────────────────────────────────────────────────────────
Write-Host ""
Write-Host "========================================" -ForegroundColor Green
Write-Host "  komari-agent-rs v$tag installed" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
Write-Host "  Binary:     $binPath"
Write-Host "  Config:     $configPath"
Write-Host "  Task:       $TaskName (runs as SYSTEM at startup)"
Write-Host "  Arguments:  $argString"
if (-not $Token -or -not $Endpoint) {
    Write-Host ""
    Write-Host "  [NOTE] Edit $configPath with your token" -ForegroundColor Yellow
    Write-Host "         and endpoint, then restart the task:" -ForegroundColor Yellow
    Write-Host "           Restart-ScheduledTask -TaskName '$TaskName'" -ForegroundColor Yellow
}
Write-Host "========================================" -ForegroundColor Green
