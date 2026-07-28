[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PayloadPath,
    [Parameter(Mandatory)][string]$SnapshotPath,
    [Parameter(Mandatory)][string]$ProofPath,
    [Parameter(Mandatory)][string]$IncludePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-StableRegularFileFingerprint {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Description
    )

    $normalizedPath = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $normalizedPath -PathType Leaf)) {
        throw "$Description is missing or is not a regular file: $normalizedPath"
    }

    $before = Get-Item -LiteralPath $normalizedPath -Force
    if ($before.PSIsContainer -or (($before.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Description must be a regular non-reparse file: $normalizedPath"
    }
    if ($before.Length -le 0) {
        throw "$Description is empty: $normalizedPath"
    }

    $sha256 = (Get-FileHash -LiteralPath $before.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $after = Get-Item -LiteralPath $before.FullName -Force
    if ($after.PSIsContainer -or (($after.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Description became unsafe while hashing: $normalizedPath"
    }
    if ($before.Length -ne $after.Length -or $before.LastWriteTimeUtc.Ticks -ne $after.LastWriteTimeUtc.Ticks) {
        throw "$Description changed while its SHA-256 was being calculated: $normalizedPath"
    }
    if ($sha256 -notmatch '^[0-9a-f]{64}$') {
        throw "Could not calculate a valid SHA-256 for ${Description}: $normalizedPath"
    }

    return [pscustomobject]@{
        Path         = $after.FullName
        Sha256       = $sha256
        Length       = [int64]$after.Length
        LastWriteUtc = $after.LastWriteTimeUtc
    }
}

function Resolve-DirectOutputPath {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Description,
        [Parameter(Mandatory)][string]$WorkingDirectory
    )

    $normalizedPath = if ([IO.Path]::IsPathRooted($Path)) {
        [IO.Path]::GetFullPath($Path)
    }
    else {
        [IO.Path]::GetFullPath((Join-Path $WorkingDirectory $Path))
    }
    $parent = [IO.Path]::GetDirectoryName($normalizedPath)
    if (-not [string]::Equals($parent, $WorkingDirectory, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description must be a direct child of the makensis working directory '$WorkingDirectory': $normalizedPath"
    }

    $name = [IO.Path]::GetFileName($normalizedPath)
    if ($name -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$') {
        throw "$Description has an unsafe file name: '$name'."
    }
    if (Test-Path -LiteralPath $normalizedPath) {
        $existing = Get-Item -LiteralPath $normalizedPath -Force
        if ($existing.PSIsContainer -or (($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "Refusing to replace an unsafe $Description path: $normalizedPath"
        }
    }
    return $normalizedPath
}

function Write-AtomicTextFile {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content,
        [Parameter(Mandatory)][Text.Encoding]$Encoding
    )

    $parent = [IO.Path]::GetDirectoryName($Path)
    $temporaryPath = Join-Path $parent (".$([IO.Path]::GetFileName($Path)).$([guid]::NewGuid().ToString('N')).tmp")
    try {
        [IO.File]::WriteAllText($temporaryPath, $Content, $Encoding)
        Move-Item -LiteralPath $temporaryPath -Destination $Path -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporaryPath) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
}

$workingDirectory = [IO.Path]::GetFullPath((Get-Location).Path)
$payload = Get-StableRegularFileFingerprint -Path $PayloadPath -Description 'Tauri NSS-patched main binary'
$snapshot = Resolve-DirectOutputPath -Path $SnapshotPath -Description 'NSIS payload snapshot' -WorkingDirectory $workingDirectory
$proof = Resolve-DirectOutputPath -Path $ProofPath -Description 'NSIS payload proof' -WorkingDirectory $workingDirectory
$include = Resolve-DirectOutputPath -Path $IncludePath -Description 'NSIS payload proof include' -WorkingDirectory $workingDirectory

# Tauri's generated template copies MAINBINARYSRCPATH without `/oname`.
# Enforce this invariant inside the helper as well as the hook so a renamed
# snapshot can never compile into a side-by-side file while leaving an older
# installed executable untouched.
if (-not [string]::Equals(
        [IO.Path]::GetFileName($snapshot),
        [IO.Path]::GetFileName($payload.Path),
        [StringComparison]::OrdinalIgnoreCase
    )) {
    throw "NSIS payload snapshot must preserve the main binary file name '$([IO.Path]::GetFileName($payload.Path))': $snapshot"
}

$resolvedOutputs = @($snapshot, $proof, $include)
if (@($resolvedOutputs | Sort-Object -Unique).Count -ne $resolvedOutputs.Count) {
    throw 'NSIS payload snapshot, proof, and include paths must be distinct.'
}

$snapshotTemporaryPath = Join-Path $workingDirectory (".$([IO.Path]::GetFileName($snapshot)).$([guid]::NewGuid().ToString('N')).tmp")
try {
    [IO.File]::Copy($payload.Path, $snapshotTemporaryPath, $false)
    $temporarySnapshot = Get-StableRegularFileFingerprint -Path $snapshotTemporaryPath -Description 'temporary NSIS payload snapshot'
    $payloadAfterCopy = Get-StableRegularFileFingerprint -Path $payload.Path -Description 'Tauri NSS-patched main binary after snapshot'
    if (
        -not [string]::Equals($payload.Sha256, $payloadAfterCopy.Sha256, [StringComparison]::OrdinalIgnoreCase) -or
        $payload.Length -ne $payloadAfterCopy.Length -or
        -not [string]::Equals($payload.Sha256, $temporarySnapshot.Sha256, [StringComparison]::OrdinalIgnoreCase) -or
        $payload.Length -ne $temporarySnapshot.Length
    ) {
        throw 'The Tauri NSS-patched main binary changed while its immutable NSIS payload snapshot was created.'
    }

    Move-Item -LiteralPath $snapshotTemporaryPath -Destination $snapshot -Force
}
finally {
    if (Test-Path -LiteralPath $snapshotTemporaryPath) {
        Remove-Item -LiteralPath $snapshotTemporaryPath -Force
    }
}

$snapshotFingerprint = Get-StableRegularFileFingerprint -Path $snapshot -Description 'immutable NSIS payload snapshot'
if (
    -not [string]::Equals($snapshotFingerprint.Sha256, $payload.Sha256, [StringComparison]::OrdinalIgnoreCase) -or
    $snapshotFingerprint.Length -ne $payload.Length
) {
    throw 'The published NSIS payload snapshot does not match the Tauri NSS-patched main binary.'
}

$nonceBytes = New-Object byte[] 16
$random = [Security.Cryptography.RandomNumberGenerator]::Create()
try {
    $random.GetBytes($nonceBytes)
}
finally {
    $random.Dispose()
}
$nonce = ([BitConverter]::ToString($nonceBytes)).Replace('-', '').ToLowerInvariant()

$proofPayload = [ordered]@{
    schemaVersion    = 1
    managedBy        = 'iHub NSIS payload proof v1'
    payloadSha256    = $snapshotFingerprint.Sha256
    payloadLength    = $snapshotFingerprint.Length
    nonce            = $nonce
    snapshotFileName = [IO.Path]::GetFileName($snapshot)
    generatedAt      = [DateTime]::UtcNow.ToString('o')
}
$proofJson = $proofPayload | ConvertTo-Json -Depth 3 -Compress
$includeSource = @(
    "!define IHUB_NSIS_PAYLOAD_SHA256 `"$($snapshotFingerprint.Sha256)`""
    "!define IHUB_NSIS_PAYLOAD_LENGTH `"$($snapshotFingerprint.Length)`""
    "!define IHUB_NSIS_PAYLOAD_NONCE `"$nonce`""
    "!define IHUB_NSIS_PAYLOAD_SNAPSHOT `"$([IO.Path]::GetFileName($snapshot))`""
) -join "`r`n"

Write-AtomicTextFile -Path $proof -Content $proofJson -Encoding ([Text.UTF8Encoding]::new($false))
Write-AtomicTextFile -Path $include -Content $includeSource -Encoding ([Text.ASCIIEncoding]::new())

Write-Host "Prepared immutable NSIS main-binary payload proof: $($snapshotFingerprint.Sha256)"
