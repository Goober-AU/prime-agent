$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot
$tokenPath = Join-Path $root 'local-token.txt'
$healthUri = 'http://127.0.0.1:43120/health'
$taskName = 'OptimusAgentCloudModelGateway'
$expectedVersion = 2
$expectedBuildId = 'optimus-gateway-monitoring-repair-20260918.1'

$localToken = (Get-Content -LiteralPath $tokenPath -Raw).Trim()
if ([string]::IsNullOrWhiteSpace($localToken)) {
    throw 'Cloud model gateway local token is missing.'
}
$headers = @{ Authorization = "Bearer $localToken" }
$lastHealthError = $null

function Test-GatewayHealth {
    try {
        $response = Invoke-RestMethod -Uri $healthUri -Headers $headers -Method Get -TimeoutSec 1
        $script:lastHealthError = if ($response.status -ne 'ok') { "status=$($response.status)" } elseif ($response.version -ne $expectedVersion -or $response.buildId -ne $expectedBuildId) { "unexpected build version=$($response.version) build=$($response.buildId)" } else { $null }
        return $response.status -eq 'ok' -and $response.version -eq $expectedVersion -and $response.buildId -eq $expectedBuildId
    } catch {
        $script:lastHealthError = $_.Exception.Message
        return $false
    }
}

$healthy = Test-GatewayHealth
if (-not $healthy -and $lastHealthError -notlike 'unexpected build*') {
    # Avoid disrupting unrelated in-flight model calls because of one transient
    # local health timeout. A confirmed stale build still restarts immediately.
    foreach ($delayMs in @(400, 800)) {
        Start-Sleep -Milliseconds $delayMs
        if (Test-GatewayHealth) {
            $healthy = $true
            break
        }
    }
}

if (-not $healthy) {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        throw "Cloud model gateway task is missing. Run install-scheduled-task.ps1 once."
    }
    if ($task.State -eq 'Running') {
        throw "Refusing to restart a running gateway automatically ($lastHealthError). Use the guarded cutover or rollback transaction after proving a no-request window."
    }
    Start-ScheduledTask -TaskName $taskName

    $deadline = [DateTime]::UtcNow.AddSeconds(8)
    do {
        Start-Sleep -Milliseconds 150
        if (Test-GatewayHealth) {
            break
        }
    } while ([DateTime]::UtcNow -lt $deadline)
}

if (-not (Test-GatewayHealth)) {
    throw "Cloud model gateway did not become healthy ($lastHealthError). See $(Join-Path $root 'gateway.log')"
}

# Optimus captures this single line and uses it only to authenticate to localhost.
Write-Output $localToken
