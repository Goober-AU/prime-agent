$ErrorActionPreference = 'Stop'

$moduleRoot = Join-Path $PSScriptRoot 'trusted-modules\Az.Accounts\5.3.4'
$expectedFiles = @{
    'Az.Accounts.psd1' = '3A113A45D62EA744BE91E22BCE385D3465055A18D52444492156B8738094316C'
    'Az.Accounts.psm1' = '30F6F7FF701E2694FD7F659524DFFE861BF9E20954366F00FB9E2F755815F75C'
    'Microsoft.Azure.PowerShell.Authentication.dll' = 'BB1CF007B39B1693712908F12BB0E065E42CD25E7AF4B0AF8293ED594F827C9A'
}

foreach ($entry in $expectedFiles.GetEnumerator()) {
    $path = Join-Path $moduleRoot $entry.Key
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Trusted Az.Accounts file is missing: $path"
    }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    if ($actual -ne $entry.Value) {
        throw "Trusted Az.Accounts integrity check failed: $path"
    }
    if ((Get-AuthenticodeSignature -LiteralPath $path).Status -ne 'Valid') {
        throw "Trusted Az.Accounts signature check failed: $path"
    }
}

Import-Module -Name (Join-Path $moduleRoot 'Az.Accounts.psd1') -Force -ErrorAction Stop
