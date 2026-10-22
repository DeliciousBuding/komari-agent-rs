# Windows installer. Downloads and verifies before touching a running installation.
param(
    [string]$Token,
    [string]$Endpoint,
    [string]$Version = "",
    [string]$InstallDir = "$env:ProgramData\komari-agent",
    [string]$GitHubProxy = ""
)
$ErrorActionPreference = "Stop"

function Get-AgentAsset([string]$Architecture) {
    if ($Architecture -ne 'AMD64') { throw 'No Windows release asset for this architecture (x86_64 only).' }
    return 'komari-agent-rs-windows-x86_64.exe'
}

function Get-AgentChecksum([string]$Text, [string]$Asset) {
    $matchesFound = @($Text -split "\r?\n" | ForEach-Object {
        $parts = $_.Trim() -split '\s+'
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -ceq $Asset -and $parts[0] -match '^[a-fA-F0-9]{64}$') {
            $parts[0].ToLowerInvariant()
        }
    })
    if ($matchesFound.Count -ne 1) { throw 'Missing, duplicate, or malformed checksum entry; refusing installation.' }
    return $matchesFound[0]
}

function Write-AgentConfig([string]$Path, [string]$NewToken, [string]$NewEndpoint) {
    if (Test-Path -LiteralPath $Path) {
        $config = Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
    } else {
        $config = [pscustomobject]@{endpoint='';token='';disable_web_ssh=$true;disable_exec=$true;disable_auto_update=$true}
    }
    if ($NewToken) { $config | Add-Member -NotePropertyName token -NotePropertyValue $NewToken -Force }
    if ($NewEndpoint) { $config | Add-Member -NotePropertyName endpoint -NotePropertyValue $NewEndpoint -Force }
    $json = $config | ConvertTo-Json -Depth 32
    [IO.File]::WriteAllText($Path, $json, [Text.UTF8Encoding]::new($false))
    return [bool]($config.token -and $config.endpoint)
}

function Test-AgentAdministrator {
    $admin = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    return $admin.IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
}

function Protect-AgentInstallDirectory([string]$Path) {
    & icacls.exe $Path /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not protect installation directory.' }
}

function Install-KomariAgent {
    if (-not (Test-AgentAdministrator)) {
        throw 'Run this installer as Administrator.'
    }
    $asset = Get-AgentAsset $env:PROCESSOR_ARCHITECTURE
    $repo = 'DeliciousBuding/komari-agent-rs'
    $tag = $Version
    if (-not $tag) {
        $tag = (Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -UseBasicParsing).tag_name
    }
    if ($tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+([.-][a-zA-Z0-9.-]+)?$') { throw 'Invalid release tag.' }
    $base = "https://github.com/$repo/releases/download/$tag"
    if ($GitHubProxy) {
        if ($GitHubProxy -notmatch '^https://') { throw 'GitHub proxy must use HTTPS.' }
        $base = "$($GitHubProxy.TrimEnd('/'))/$base"
    }
    $stage = Join-Path ([IO.Path]::GetTempPath()) ("komari-install-" + [Guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $stage
    $bin = Join-Path $InstallDir 'komari-agent-rs.exe'
    $cfg = Join-Path $InstallDir 'config.json'
    $taskName = 'komari-agent-rs'
    $oldTaskXml = $null
    $wasRunning = $false
    $backupBin = $null
    $backupConfig = $null
    $changed = $false
    try {
        $download = Join-Path $stage $asset
        Invoke-WebRequest -Uri "$base/$asset" -OutFile $download -UseBasicParsing
        $response = Invoke-WebRequest -Uri "$base/checksums.txt" -UseBasicParsing
        $text = if ($response.Content -is [byte[]]) { [Text.Encoding]::UTF8.GetString($response.Content) } else { [string]$response.Content }
        $expected = Get-AgentChecksum $text $asset
        if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
            throw 'SHA256 mismatch; existing installation has not been changed.'
        }
        $null = New-Item -ItemType Directory -Path $InstallDir -Force
        # Config contains a credential: limit the installation to SYSTEM/admins.
        Protect-AgentInstallDirectory $InstallDir
        $existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        if ($existing) {
            $oldTaskXml = Export-ScheduledTask -TaskName $taskName
            $wasRunning = $existing.State -eq 'Running'
        }
        $stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ')
        if (Test-Path -LiteralPath $bin) {
            $backupBin = "$bin.pre-$stamp"
            Copy-Item -LiteralPath $bin -Destination $backupBin
        }
        if (Test-Path -LiteralPath $cfg) { $backupConfig = [IO.File]::ReadAllBytes($cfg) }
        if ($existing) { Stop-ScheduledTask -TaskName $taskName }
        $changed = $true
        Copy-Item -LiteralPath $download -Destination $bin -Force
        $ready = Write-AgentConfig $cfg $Token $Endpoint
        $escapedBin = [Security.SecurityElement]::Escape($bin)
        $escapedArgs = [Security.SecurityElement]::Escape('--config "' + $cfg + '"')
        if ($oldTaskXml) {
            [xml]$xml = $oldTaskXml
            $xml.Task.Actions.Exec.Command = $bin
            $xml.Task.Actions.Exec.Arguments = '--config "' + $cfg + '"'
            $taskXml = $xml.OuterXml
        } else {
            $taskXml = @"
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<Triggers><BootTrigger><Enabled>true</Enabled></BootTrigger></Triggers>
<Principals><Principal id="System"><UserId>S-1-5-18</UserId><LogonType>ServiceAccount</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>5</Count></RestartOnFailure></Settings>
<Actions Context="System"><Exec><Command>$escapedBin</Command><Arguments>$escapedArgs</Arguments></Exec></Actions>
</Task>
"@
        }
        Register-ScheduledTask -TaskName $taskName -Xml $taskXml -Force | Out-Null
        if ($ready) { Start-ScheduledTask -TaskName $taskName }
        Write-Host "Installed $tag; SHA256 $expected"
        Write-Host "Config: $cfg (existing fields preserved; credentials are not task arguments)"
        if (-not $ready) { Write-Host 'Set endpoint/token in the config before starting the task.' }
    } catch {
        if ($changed) {
            try { Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue } catch { }
            if ($backupBin) { Copy-Item -LiteralPath $backupBin -Destination $bin -Force }
            if ($backupConfig) { [IO.File]::WriteAllBytes($cfg, $backupConfig) }
            if ($oldTaskXml) {
                Register-ScheduledTask -TaskName $taskName -Xml $oldTaskXml -Force | Out-Null
                if ($wasRunning) { Start-ScheduledTask -TaskName $taskName }
            }
        }
        throw
    } finally {
        # Only the unique staging directory created above; never the install directory.
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
}
# Dot-sourcing exposes pure helpers for offline tests without installing anything.
if ($MyInvocation.InvocationName -ne '.') { Install-KomariAgent }
