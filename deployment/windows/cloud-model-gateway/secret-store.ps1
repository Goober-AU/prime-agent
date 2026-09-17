param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('set', 'get', 'exists')]
    [string]$Action,

    [Parameter(Mandatory = $true, Position = 1)]
    [ValidateSet('ollama')]
    [string]$Name
)

$ErrorActionPreference = 'Stop'
$secretDirectory = Join-Path $PSScriptRoot 'secrets'
$secretPath = Join-Path $secretDirectory "$Name.credential.xml"

if ($Action -eq 'set') {
    New-Item -ItemType Directory -Path $secretDirectory -Force | Out-Null
    $secureValue = Read-Host "Enter the $Name API key" -AsSecureString
    $credential = [System.Management.Automation.PSCredential]::new($Name, $secureValue)
    if ([string]::IsNullOrWhiteSpace($credential.GetNetworkCredential().Password)) {
        throw 'The API key cannot be empty.'
    }
    $credential | Export-Clixml -LiteralPath $secretPath -Force
    Write-Output 'stored'
    exit 0
}

if ($Action -eq 'exists') {
    if (Test-Path -LiteralPath $secretPath) {
        Write-Output 'true'
        exit 0
    }
    Write-Output 'false'
    exit 1
}

if (-not (Test-Path -LiteralPath $secretPath)) {
    throw "No stored credential exists for $Name."
}

$storedCredential = Import-Clixml -LiteralPath $secretPath
$plainValue = $storedCredential.GetNetworkCredential().Password
if ([string]::IsNullOrWhiteSpace($plainValue)) {
    throw "The stored credential for $Name is empty or unreadable."
}
Write-Output $plainValue
