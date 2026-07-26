[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$InputDirectory,

    [string]$OutputFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$inputRoot = (Resolve-Path -LiteralPath $InputDirectory).Path
if (-not [System.IO.Directory]::Exists($inputRoot)) {
    throw "InputDirectory is not a directory: $InputDirectory"
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $inputRoot 'SHA256SUMS.txt'
}

$releaseFiles = @(
    Get-ChildItem -LiteralPath $inputRoot -File -Recurse |
        Where-Object { $_.Name -notin @('SHA256SUMS.txt', 'latest.json') } |
        Sort-Object -Property Name
)

if ($releaseFiles.Count -eq 0) {
    throw "No release assets found under: $inputRoot"
}

$duplicates = @($releaseFiles | Group-Object -Property Name | Where-Object Count -gt 1)
if ($duplicates.Count -gt 0) {
    $names = ($duplicates | ForEach-Object Name) -join ', '
    throw "Checksum manifest cannot represent duplicate release asset names: $names"
}

$lines = foreach ($file in $releaseFiles) {
    $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash *$($file.Name)"
}

$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllLines($OutputFile, [string[]]$lines, $utf8WithoutBom)
Write-Host "Wrote $($lines.Count) SHA-256 checksums to $OutputFile"
