# Offline installer regressions: no admin, network, real tasks or agents.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../install.ps1"
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
$root = Join-Path ([IO.Path]::GetTempPath()) ('komari-installer-test-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $root
$previousArch = $env:PROCESSOR_ARCHITECTURE
try {
    Assert ((Get-AgentAsset 'AMD64') -eq 'komari-agent-rs-windows-x86_64.exe') 'asset mapping'
    try { Get-AgentAsset 'ARM64'; throw 'accepted unsupported architecture' } catch { Assert ($_.Exception.Message -like '*No Windows release asset*') 'unsupported architecture rejection' }
    $hash = 'a' * 64
    Assert ((Get-AgentChecksum "$hash *target`n$('b'*64)  target-full" 'target') -eq $hash) 'exact checksum match'
    foreach ($text in @('', 'bad  target', "$hash  target`n$hash  target")) {
        $failed=$false
        try { Get-AgentChecksum $text 'target' } catch { $failed=$true }
        Assert $failed 'missing/invalid/duplicate checksum accepted'
    }
    $script:InstallDir = Join-Path $root 'install with spaces'
    $null = New-Item -ItemType Directory -Path $InstallDir
    $bin = Join-Path $InstallDir 'komari-agent-rs.exe'
    $cfg = Join-Path $InstallDir 'config.json'
    [IO.File]::WriteAllText($bin,'old-binary')
    [IO.File]::WriteAllText($cfg,'{"token":"synthetic-secret-canary","endpoint":"https://example.test","include_nics":["fixture"],"disable_exec":true}')
    $oldConfig = [IO.File]::ReadAllText($cfg)
    $script:Token = ''; $script:Endpoint = ''; $script:Version='v0.6.0'; $script:GitHubProxy=''
    $env:PROCESSOR_ARCHITECTURE='AMD64'
    $script:mode='valid'; $script:stopped=0; $script:started=0; $script:registered=@(); $script:failRegister=$false
    function Test-AgentAdministrator { return $true }
    function Protect-AgentInstallDirectory([string]$Path) { }
    function Wait-AgentStopped([string]$Task, [string]$Binary) { }
    function Get-ScheduledTask { return [pscustomobject]@{State='Running'} }
    function Export-ScheduledTask { return '<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Actions><Exec><Command>old</Command><Arguments>--config "old"</Arguments></Exec></Actions></Task>' }
    function Stop-ScheduledTask { $script:stopped++ }
    function Start-ScheduledTask { $script:started++ }
    function Register-ScheduledTask {
        param($TaskName,$Xml,[switch]$Force)
        if ($script:failRegister) { $script:failRegister=$false; throw 'synthetic registration failure' }
        $script:registered += $Xml
    }
    function Invoke-WebRequest {
        param($Uri,$OutFile,[switch]$UseBasicParsing)
        if ($Uri.EndsWith('/checksums.txt')) {
            if ($script:mode -eq 'missing') { throw 'synthetic checksum download failure' }
            $bytes=[Text.Encoding]::UTF8.GetBytes('new-binary')
            $sha=[Security.Cryptography.SHA256]::Create()
            $value=([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-','').ToLowerInvariant()
            $sha.Dispose()
            if ($script:mode -eq 'mismatch') { $value='0'*64 }
            return [pscustomobject]@{Content="$value  komari-agent-rs-windows-x86_64.exe"}
        }
        [IO.File]::WriteAllText($OutFile,'new-binary',[Text.UTF8Encoding]::new($false))
    }
    foreach ($mode in @('missing','mismatch')) {
        $script:mode=$mode; $failed=$false
        try { Install-KomariAgent } catch { $failed=$true }
        Assert $failed 'verification failure accepted'
        Assert ($script:stopped -eq 0) 'task touched before verification'
        Assert ([IO.File]::ReadAllText($bin) -eq 'old-binary') 'old binary changed on verification failure'
        Assert ([IO.File]::ReadAllText($cfg) -eq $oldConfig) 'old config changed on verification failure'
    }
    $script:mode='valid'; $script:failRegister=$true; $failed=$false
    try { Install-KomariAgent } catch { $failed=$true }
    Assert $failed 'registration failure accepted'
    Assert ([IO.File]::ReadAllText($bin) -eq 'old-binary') 'binary rollback failed'
    Assert ([IO.File]::ReadAllText($cfg) -eq $oldConfig) 'config rollback failed'
    $output = Install-KomariAgent 6>&1 | Out-String
    Assert (-not $output.Contains('synthetic-secret-canary')) 'credential leaked in output'
    $saved=Get-Content -Raw -LiteralPath $cfg | ConvertFrom-Json
    Assert ($saved.token -eq 'synthetic-secret-canary' -and $saved.include_nics[0] -eq 'fixture' -and $saved.disable_exec) 'existing configuration was not preserved'
    Assert ([IO.File]::ReadAllBytes($cfg)[0] -ne 0xEF) 'config has UTF-8 BOM'
    Assert (-not $script:registered[-1].Contains('synthetic-secret-canary')) 'credential leaked in task action'
    Assert ($script:registered[-1].Contains('install with spaces')) 'config path missing'
    Write-Host 'Installer regressions passed (offline).'
} finally {
    $env:PROCESSOR_ARCHITECTURE=$previousArch
    # root is an absolute, unique directory created by this test.
    Remove-Item -LiteralPath $root -Recurse -Force
}
