# iHub bootstrap installer for supported 64-bit Windows releases.
# The downloaded installer is always verified against SHA256SUMS.txt from the
# selected GitHub Release before it is executed.

[CmdletBinding()]
param(
    [string]$Repository,

    [string]$Version,

    [switch]$Interactive,

    [switch]$RequireAuthenticodeSignature
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-Repository {
    param([Parameter(Mandatory)][string]$Value)

    if ($Value -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$') {
        throw 'Repository must be in owner/repository form.'
    }

    return $Value
}

function Assert-ReleaseTag {
    param([Parameter(Mandatory)][string]$Value)

    if ($Value -eq 'latest') {
        return $Value
    }

    if ($Value -notmatch '^v?[0-9A-Za-z][0-9A-Za-z.+-]*$') {
        throw 'Version must be "latest" or a simple release tag such as v0.1.0.'
    }

    return $Value
}

function Get-Release {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string]$Tag
    )

    $headers = @{
        Accept                 = 'application/vnd.github+json'
        'X-GitHub-Api-Version' = '2022-11-28'
        'User-Agent'           = 'iHub-installer'
    }
    $apiRoot = "https://api.github.com/repos/$Repo/releases"
    $uri = if ($Tag -eq 'latest') {
        "$apiRoot/latest"
    }
    else {
        "$apiRoot/tags/$([uri]::EscapeDataString($Tag))"
    }

    try {
        $release = Invoke-RestMethod -Uri $uri -Headers $headers
    }
    catch {
        throw "Could not retrieve iHub release '$Tag' from '$Repo': $($_.Exception.Message)"
    }

    if ($release.draft -eq $true) {
        throw 'Refusing to install from a draft GitHub Release.'
    }

    return $release
}

function Get-ReleaseAsset {
    param(
        [Parameter(Mandatory)]$Release,
        [Parameter(Mandatory)][scriptblock]$Predicate,
        [Parameter(Mandatory)][string]$Description
    )

    $assets = @($Release.assets | Where-Object $Predicate)
    if ($assets.Count -ne 1) {
        $available = (@($Release.assets | ForEach-Object { $_.name }) -join ', ')
        throw "Expected exactly one $Description asset; found $($assets.Count). Available assets: $available"
    }

    return $assets[0]
}

function Invoke-Download {
    param(
        [Parameter(Mandatory)][string]$Url,
        [Parameter(Mandatory)][string]$Destination
    )

    $uri = [uri]$Url
    if ($uri.Scheme -ne 'https') {
        throw "Refusing a non-HTTPS download URL: $Url"
    }

    Invoke-WebRequest -Uri $uri.AbsoluteUri -OutFile $Destination -Headers @{
        'User-Agent' = 'iHub-installer'
    }
}

function Get-ExpectedChecksum {
    param(
        [Parameter(Mandatory)][string]$ChecksumFile,
        [Parameter(Mandatory)][string]$AssetName
    )

    foreach ($line in (Get-Content -LiteralPath $ChecksumFile)) {
        $match = [regex]::Match($line.Trim(), '^([0-9a-fA-F]{64})\s+\*?(.+)$')
        if ($match.Success -and $match.Groups[2].Value.Trim() -eq $AssetName) {
            return $match.Groups[1].Value.ToLowerInvariant()
        }
    }

    throw "SHA256SUMS.txt does not contain a checksum for '$AssetName'."
}

if ([Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'scripts/install.ps1 installs iHub on Windows only. Use scripts/install.sh on macOS.'
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'iHub releases currently require a 64-bit Windows installation.'
}

# Windows PowerShell 5.1 may otherwise negotiate an obsolete TLS protocol.
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}
catch {
    Write-Verbose 'Could not explicitly enable TLS 1.2; continuing with the system default.'
}

if ([string]::IsNullOrWhiteSpace($Repository)) {
    $Repository = if ([string]::IsNullOrWhiteSpace($env:IHUB_REPOSITORY)) { 'neko233-com/ihub' } else { $env:IHUB_REPOSITORY }
}
if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = if ([string]::IsNullOrWhiteSpace($env:IHUB_VERSION)) { 'latest' } else { $env:IHUB_VERSION }
}

$Repository = Assert-Repository $Repository
$Version = Assert-ReleaseTag $Version
$release = Get-Release -Repo $Repository -Tag $Version

$installerAsset = Get-ReleaseAsset -Release $release -Description 'Windows x64 NSIS installer' -Predicate {
    param($asset)
    [string]$asset.name -match '(?i)^ihub_[^_]+_windows_x64_setup\.exe$'
}
$checksumAsset = Get-ReleaseAsset -Release $release -Description 'SHA-256 manifest' -Predicate {
    param($asset)
    [string]$asset.name -eq 'SHA256SUMS.txt'
}

$installerName = [string]$installerAsset.name
if ([IO.Path]::GetFileName($installerName) -ne $installerName) {
    throw "Refusing an installer asset with an unsafe name: $installerName"
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("ihub-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
    $installerPath = Join-Path $tempRoot $installerName
    $checksumPath = Join-Path $tempRoot 'SHA256SUMS.txt'

    Write-Host "Downloading iHub $($release.tag_name) for Windows x64..."
    Invoke-Download -Url ([string]$checksumAsset.browser_download_url) -Destination $checksumPath
    Invoke-Download -Url ([string]$installerAsset.browser_download_url) -Destination $installerPath

    $expectedHash = Get-ExpectedChecksum -ChecksumFile $checksumPath -AssetName $installerName
    $actualHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw "SHA-256 verification failed for '$installerName'. Expected $expectedHash, got $actualHash."
    }
    Write-Host 'SHA-256 verification passed.'

    $signature = Get-AuthenticodeSignature -LiteralPath $installerPath
    if ($signature.Status -eq 'Valid') {
        Write-Host "Authenticode verification passed: $($signature.SignerCertificate.Subject)"
    }
    elseif ($RequireAuthenticodeSignature) {
        throw "Authenticode verification failed with status '$($signature.Status)'."
    }
    else {
        Write-Warning "Installer Authenticode status is '$($signature.Status)'. The SHA-256 manifest is valid, but use -RequireAuthenticodeSignature for a signed-only policy."
    }

    if ($Interactive) {
        $process = Start-Process -FilePath $installerPath -Wait -PassThru
    }
    else {
        # Tauri's NSIS installer supports an unattended per-user installation.
        $process = Start-Process -FilePath $installerPath -ArgumentList '/S' -Wait -PassThru
    }

    if ($process.ExitCode -notin @(0, 3010)) {
        throw "The iHub installer exited with code $($process.ExitCode)."
    }
    if ($process.ExitCode -eq 3010) {
        Write-Warning 'iHub installed successfully; Windows requested a restart.'
    }
    else {
        Write-Host 'iHub installed successfully.'
    }
}
finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
