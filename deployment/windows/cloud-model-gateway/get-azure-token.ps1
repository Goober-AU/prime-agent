param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('https://ai.azure.com', 'https://cognitiveservices.azure.com')]
    [string]$Resource
)

$ErrorActionPreference = 'Stop'
$WarningPreference = 'SilentlyContinue'

$deployment = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'deployment.json') -Raw | ConvertFrom-Json
$tenantId = [string]$deployment.tenantId
$subscriptionId = [string]$deployment.subscriptionId
$guidPattern = '^[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$'
if ($tenantId -notmatch $guidPattern -or $subscriptionId -notmatch $guidPattern) { throw 'Invalid Azure deployment identity.' }

. (Join-Path $PSScriptRoot 'Import-TrustedAzAccounts.ps1')
$context = Get-AzContext -ErrorAction Stop
if ($null -eq $context -or $null -eq $context.Account) {
    throw "Azure login is missing. Run cloud-model-gateway\azure-login.ps1, then retry the model request."
}

Set-AzContext -SubscriptionId $subscriptionId -TenantId $tenantId -Scope Process -ErrorAction Stop -WarningAction SilentlyContinue | Out-Null
$token = Get-AzAccessToken -ResourceUrl $Resource -TenantId $tenantId -ErrorAction Stop
$plainToken = if ($token.Token -is [System.Security.SecureString]) {
    [System.Net.NetworkCredential]::new('', $token.Token).Password
} else {
    [string]$token.Token
}

if ([string]::IsNullOrWhiteSpace($plainToken)) {
    throw "Azure returned an empty access token for $Resource."
}

$expiresOn = if ($token.ExpiresOn -is [DateTimeOffset]) {
    $token.ExpiresOn.UtcDateTime.ToString('o')
} else {
    ([DateTimeOffset]$token.ExpiresOn).UtcDateTime.ToString('o')
}

[pscustomobject]@{
    accessToken = $plainToken
    expiresOn = $expiresOn
    tenant = $tenantId
    subscription = $subscriptionId
} | ConvertTo-Json -Compress
