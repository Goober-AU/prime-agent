$ErrorActionPreference = 'Stop'

$deployment = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'deployment.json') -Raw | ConvertFrom-Json
$tenantId = [string]$deployment.tenantId
$subscriptionId = [string]$deployment.subscriptionId
$guidPattern = '^[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$'
if ($tenantId -notmatch $guidPattern -or $subscriptionId -notmatch $guidPattern) { throw 'Invalid Azure deployment identity.' }

. (Join-Path $PSScriptRoot 'Import-TrustedAzAccounts.ps1')

Connect-AzAccount `
    -UseDeviceAuthentication `
    -Tenant $tenantId `
    -Subscription $subscriptionId `
    -Scope CurrentUser `
    -ErrorAction Stop | Out-Null

Set-AzContext `
    -SubscriptionId $subscriptionId `
    -TenantId $tenantId `
    -Scope CurrentUser `
    -ErrorAction Stop `
    -WarningAction SilentlyContinue | Out-Null

$tokenHelper = Join-Path $PSScriptRoot 'get-azure-token.ps1'
$validatedTokens = foreach ($resource in @('https://ai.azure.com', 'https://cognitiveservices.azure.com')) {
    $tokenData = (& $tokenHelper -Resource $resource) | ConvertFrom-Json -ErrorAction Stop
    if ([string]::IsNullOrWhiteSpace([string]$tokenData.accessToken)) {
        throw "Azure token validation returned an empty access token for $resource."
    }
    $expiresOn = [DateTimeOffset]::Parse([string]$tokenData.expiresOn).ToUniversalTime()
    if ($expiresOn -le [DateTimeOffset]::UtcNow) {
        throw "Azure token validation returned an expired token for $resource."
    }
    [pscustomobject]@{
        resource = $resource
        expiresOn = $expiresOn.ToString('o')
    }
}

[pscustomobject]@{
    subscription = $subscriptionId
    tenant = $tenantId
    status = 'ready'
    tokens = $validatedTokens
} | ConvertTo-Json -Compress
