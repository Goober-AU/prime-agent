$ErrorActionPreference = 'Stop'
$helper = Join-Path $PSScriptRoot 'port-isolation.ps1'
$testRoot = Join-Path (Split-Path $PSScriptRoot) ('.port-env\iso\registry-' + [guid]::NewGuid().ToString('N'))
$probe = @'
$expected = Join-Path $env:HOME '..\supervisor-owners'
$actual = $env:PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR
if ([IO.Path]::GetFullPath($actual) -ne [IO.Path]::GetFullPath($expected)) { throw 'Registry did not follow the isolated root' }
if (-not (Test-Path -LiteralPath $actual -PathType Container)) { throw 'Isolated registry directory missing' }
if ($actual -like '*\.prime\*') { throw 'Production registry inherited' }
Write-Output 'PASS: explicit private registry path and directory'
exit 0
'@
$oldValue = $env:PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR
try {
    $env:PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR = 'C:\Users\openclawuser\.prime\supervisor-owners'
    & powershell.exe -NoProfile -NonInteractive -File $helper -Root $testRoot -Command $probe
    if ($LASTEXITCODE -ne 0) { throw 'Inherited production registry was not overridden' }
    Write-Output 'PASS: inherited production registry overridden in child only'
} finally {
    $env:PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR = $oldValue
}
