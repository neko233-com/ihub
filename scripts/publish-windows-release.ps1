[CmdletBinding()]
param(
    [string]$Tag,
    [string]$Repository = 'neko233-com/ihub',
    [string]$NotesFile,
    [string]$UpdaterPrivateKeyPath,
    [string]$UpdaterPasswordPath,
    [switch]$DraftOnly,
    [switch]$PlanOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Invoke-External {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$CommandArguments
    )

    & $Executable @CommandArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE`: $Executable $($CommandArguments -join ' ')"
    }
}

function Invoke-ExternalCapture {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$CommandArguments
    )

    $output = @(& $Executable @CommandArguments 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE`: $Executable $($CommandArguments -join ' ')`n$($output -join [Environment]::NewLine)"
    }
    return ($output -join [Environment]::NewLine).Trim()
}

function Test-ExternalSuccess {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$CommandArguments
    )

    & $Executable @CommandArguments *> $null
    return $LASTEXITCODE -eq 0
}

function Write-Utf8WithoutBom {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content
    )

    $encoding = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Assert-ChildPath {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Description
    )

    $resolvedRoot = [IO.Path]::GetFullPath($Root)
    $resolvedPath = [IO.Path]::GetFullPath($Path)
    $rootPrefix = $resolvedRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolvedPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description must remain below '$resolvedRoot': $resolvedPath"
    }
    return $resolvedPath
}

function Get-SingleReleaseFile {
    param(
        [Parameter(Mandatory)][string]$Directory,
        [Parameter(Mandatory)][string]$Filter,
        [Parameter(Mandatory)][string]$Description
    )

    $matches = @(Get-ChildItem -LiteralPath $Directory -File -Filter $Filter)
    if ($matches.Count -ne 1) {
        throw "Expected exactly one $Description matching '$Filter' in '$Directory', found $($matches.Count)."
    }
    return $matches[0]
}

function Assert-ReleaseConfiguration {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$ReleaseTag
    )

    if ($ReleaseTag -notmatch '^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$') {
        throw "Release tag must be SemVer in vX.Y.Z form: $ReleaseTag"
    }

    $package = Get-Content -LiteralPath (Join-Path $Root 'package.json') -Raw | ConvertFrom-Json
    $tauri = Get-Content -LiteralPath (Join-Path $Root 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
    $cargoText = Get-Content -LiteralPath (Join-Path $Root 'src-tauri\Cargo.toml') -Raw
    $cargoMatch = [regex]::Match($cargoText, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
    if (-not $cargoMatch.Success) {
        throw 'Could not read the iHub package version from src-tauri/Cargo.toml.'
    }

    $versions = @([string]$package.version, [string]$tauri.version, $cargoMatch.Groups[1].Value)
    if (@($versions | Select-Object -Unique).Count -ne 1) {
        throw "Release versions differ: package.json=$($versions[0]), tauri.conf.json=$($versions[1]), Cargo.toml=$($versions[2])."
    }
    $expectedTag = "v$($versions[0])"
    if ($ReleaseTag -ne $expectedTag) {
        throw "Release tag '$ReleaseTag' must equal source version '$expectedTag'."
    }
    if ($tauri.bundle.createUpdaterArtifacts -ne $true) {
        throw 'bundle.createUpdaterArtifacts must be true.'
    }
    if ([string]::IsNullOrWhiteSpace([string]$tauri.plugins.updater.pubkey)) {
        throw 'The Tauri updater public key is missing.'
    }
    $endpoints = @($tauri.plugins.updater.endpoints)
    if ($endpoints.Count -eq 0 -or @($endpoints | Where-Object { $_ -notmatch '^https://' }).Count -gt 0) {
        throw 'At least one HTTPS updater endpoint is required.'
    }
    return [string]$package.version
}

function Invoke-ReleaseValidation {
    param([Parameter(Mandatory)][string]$Root)

    Write-Host 'Running the complete local Windows release validation...'
    Invoke-External -Executable 'corepack' -CommandArguments @('pnpm', 'check')
    Invoke-External -Executable 'corepack' -CommandArguments @('pnpm', 'test')
    Invoke-External -Executable 'node' -CommandArguments @('--test', 'scripts/release-updater-json.node-test.mjs', 'scripts/release-assets.node-test.mjs')
    Invoke-External -Executable 'node' -CommandArguments @('scripts/verify-official-plugin-lock.mjs')
    Invoke-External -Executable 'powershell' -CommandArguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts/verify-windows-development-scripts.ps1')
    Invoke-External -Executable 'powershell' -CommandArguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts/validate-github-actions.ps1')
    Invoke-External -Executable 'cargo' -CommandArguments @('fmt', '--manifest-path', 'src-tauri/Cargo.toml', '--all', '--', '--check')
    Invoke-External -Executable 'cargo' -CommandArguments @('check', '--manifest-path', 'src-tauri/Cargo.toml', '--all-targets', '--all-features')
    Invoke-External -Executable 'cargo' -CommandArguments @('clippy', '--manifest-path', 'src-tauri/Cargo.toml', '--all-targets', '--all-features', '--', '-D', 'warnings')
    Invoke-External -Executable 'cargo' -CommandArguments @('test', '--manifest-path', 'src-tauri/Cargo.toml', '--all-features')
    Invoke-External -Executable 'git' -CommandArguments @('diff', '--check')
}

function Invoke-SignedWindowsBuild {
    param(
        [Parameter(Mandatory)][string]$KeyPath,
        [string]$PasswordPath
    )

    if (-not (Test-Path -LiteralPath $KeyPath -PathType Leaf)) {
        throw "Updater private key not found: $KeyPath"
    }
    if (-not [string]::IsNullOrWhiteSpace($PasswordPath) -and -not (Test-Path -LiteralPath $PasswordPath -PathType Leaf)) {
        throw "Updater password file not found: $PasswordPath"
    }

    $previousKey = [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY', 'Process')
    $previousPassword = [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'Process')
    try {
        $env:TAURI_SIGNING_PRIVATE_KEY = $KeyPath
        if ([string]::IsNullOrWhiteSpace($PasswordPath)) {
            Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
        }
        else {
            $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [IO.File]::ReadAllText($PasswordPath)
        }
        Invoke-External -Executable 'corepack' -CommandArguments @('pnpm', 'tauri', 'build', '--bundles', 'nsis,msi')
    }
    finally {
        if ($null -eq $previousKey) {
            Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
        }
        else {
            $env:TAURI_SIGNING_PRIVATE_KEY = $previousKey
        }
        if ($null -eq $previousPassword) {
            Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
        }
        else {
            $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $previousPassword
        }
    }
}

function Assert-DownloadedAssets {
    param(
        [Parameter(Mandatory)][string]$LocalDirectory,
        [Parameter(Mandatory)][string]$DownloadedDirectory
    )

    $localFiles = @(Get-ChildItem -LiteralPath $LocalDirectory -File | Sort-Object Name)
    $downloadedFiles = @(Get-ChildItem -LiteralPath $DownloadedDirectory -File | Sort-Object Name)
    if ($localFiles.Count -ne $downloadedFiles.Count) {
        throw "Remote release asset count differs: local=$($localFiles.Count), remote=$($downloadedFiles.Count)."
    }
    for ($index = 0; $index -lt $localFiles.Count; $index += 1) {
        if ($localFiles[$index].Name -ne $downloadedFiles[$index].Name) {
            throw "Remote release asset name differs: local=$($localFiles[$index].Name), remote=$($downloadedFiles[$index].Name)."
        }
        $localHash = (Get-FileHash -LiteralPath $localFiles[$index].FullName -Algorithm SHA256).Hash
        $remoteHash = (Get-FileHash -LiteralPath $downloadedFiles[$index].FullName -Algorithm SHA256).Hash
        if ($localHash -ne $remoteHash) {
            throw "Remote release asset hash differs: $($localFiles[$index].Name)."
        }
    }
}

$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$previousLocation = Get-Location
try {
    Set-Location -LiteralPath $root

    $package = Get-Content -LiteralPath (Join-Path $root 'package.json') -Raw | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace($Tag)) {
        $Tag = "v$($package.version)"
    }
    $version = Assert-ReleaseConfiguration -Root $root -ReleaseTag $Tag

    if ($PlanOnly) {
        Write-Host "Manual Windows release plan is valid: $Repository $Tag (Windows x64 NSIS + MSI, signed updater, local upload)."
        exit 0
    }

    if ($env:OS -ne 'Windows_NT' -or -not [Environment]::Is64BitOperatingSystem) {
        throw 'Windows x64 is required for this release script.'
    }
    foreach ($command in @('git', 'gh', 'node', 'corepack', 'cargo', 'powershell')) {
        if ($null -eq (Get-Command $command -ErrorAction SilentlyContinue)) {
            throw "Required command is unavailable: $command"
        }
    }

    $branch = Invoke-ExternalCapture -Executable 'git' -CommandArguments @('branch', '--show-current')
    if ($branch -ne 'main') {
        throw "Stable releases must run from main, not '$branch'."
    }
    $status = Invoke-ExternalCapture -Executable 'git' -CommandArguments @('status', '--porcelain=v1', '--untracked-files=all')
    if (-not [string]::IsNullOrWhiteSpace($status)) {
        throw "Stable releases require a clean worktree.`n$status"
    }

    Invoke-External -Executable 'gh' -CommandArguments @('auth', 'status')
    $actualRepository = Invoke-ExternalCapture -Executable 'gh' -CommandArguments @('repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner')
    if ($actualRepository -ne $Repository) {
        throw "Current GitHub repository is '$actualRepository', expected '$Repository'."
    }
    Invoke-External -Executable 'git' -CommandArguments @('fetch', 'origin', 'main', '--tags')
    $head = Invoke-ExternalCapture -Executable 'git' -CommandArguments @('rev-parse', 'HEAD')
    $originHead = Invoke-ExternalCapture -Executable 'git' -CommandArguments @('rev-parse', 'origin/main')
    if ($head -ne $originHead) {
        throw "HEAD $head must equal pushed origin/main $originHead."
    }

    $tagExists = Test-ExternalSuccess -Executable 'git' -CommandArguments @('show-ref', '--verify', '--quiet', "refs/tags/$Tag")
    if ($tagExists) {
        $tagCommit = Invoke-ExternalCapture -Executable 'git' -CommandArguments @('rev-list', '-n', '1', $Tag)
        if ($tagCommit -ne $head) {
            throw "Existing tag $Tag points to $tagCommit, not release commit $head. Refusing to move it."
        }
    }

    $releaseList = Invoke-ExternalCapture -Executable 'gh' -CommandArguments @('release', 'list', '--repo', $Repository, '--limit', '100', '--json', 'tagName,isDraft,isPrerelease') | ConvertFrom-Json
    $matchingReleases = @($releaseList | Where-Object { $_.tagName -eq $Tag })
    if ($matchingReleases.Count -gt 1) {
        throw "More than one GitHub Release resolved to $Tag."
    }
    if ($matchingReleases.Count -eq 1 -and $matchingReleases[0].isDraft -ne $true) {
        throw "GitHub Release $Tag is already public; refusing to replace it."
    }
    if ($matchingReleases.Count -eq 1 -and -not $tagExists) {
        throw "Draft release $Tag exists without a matching local tag."
    }

    $previousToken = [Environment]::GetEnvironmentVariable('GITHUB_TOKEN', 'Process')
    try {
        $env:GITHUB_TOKEN = Invoke-ExternalCapture -Executable 'gh' -CommandArguments @('auth', 'token')
        Invoke-External -Executable 'node' -CommandArguments @('scripts/verify-release-version.mjs', '--repository', $Repository, '--tag', $Tag)
    }
    finally {
        if ($null -eq $previousToken) {
            Remove-Item Env:GITHUB_TOKEN -ErrorAction SilentlyContinue
        }
        else {
            $env:GITHUB_TOKEN = $previousToken
        }
    }

    Invoke-ReleaseValidation -Root $root

    if ([string]::IsNullOrWhiteSpace($UpdaterPrivateKeyPath)) {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            throw 'LOCALAPPDATA is unavailable; pass -UpdaterPrivateKeyPath.'
        }
        $UpdaterPrivateKeyPath = Join-Path $env:LOCALAPPDATA 'iHub\keys\tauri-updater-release-v2.key'
    }
    $UpdaterPrivateKeyPath = [IO.Path]::GetFullPath($UpdaterPrivateKeyPath)
    if ([string]::IsNullOrWhiteSpace($UpdaterPasswordPath) -and -not [string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        $defaultPasswordPath = Join-Path $env:LOCALAPPDATA 'iHub\keys\tauri-updater-release-v2.password'
        if (Test-Path -LiteralPath $defaultPasswordPath -PathType Leaf) {
            $UpdaterPasswordPath = $defaultPasswordPath
        }
    }
    if (-not [string]::IsNullOrWhiteSpace($UpdaterPasswordPath)) {
        $UpdaterPasswordPath = [IO.Path]::GetFullPath($UpdaterPasswordPath)
    }

    Invoke-SignedWindowsBuild -KeyPath $UpdaterPrivateKeyPath -PasswordPath $UpdaterPasswordPath

    $cacheRoot = Join-Path $root '.cache\manual-release'
    $releaseRoot = Assert-ChildPath -Path (Join-Path $cacheRoot $Tag) -Root $cacheRoot -Description 'Release staging directory'
    if (Test-Path -LiteralPath $releaseRoot) {
        Remove-Item -LiteralPath $releaseRoot -Recurse -Force
    }
    $assetDirectory = Join-Path $releaseRoot 'assets'
    $downloadDirectory = Join-Path $releaseRoot 'downloaded'
    [IO.Directory]::CreateDirectory($assetDirectory) | Out-Null

    $nsisDirectory = Join-Path $root 'src-tauri\target\release\bundle\nsis'
    $msiDirectory = Join-Path $root 'src-tauri\target\release\bundle\msi'
    $nsis = Get-SingleReleaseFile -Directory $nsisDirectory -Filter "iHub_${version}_x64-setup.exe" -Description 'NSIS installer'
    $nsisSignature = Get-SingleReleaseFile -Directory $nsisDirectory -Filter "iHub_${version}_x64-setup.exe.sig" -Description 'NSIS updater signature'
    $msi = Get-SingleReleaseFile -Directory $msiDirectory -Filter "iHub_${version}_x64*.msi" -Description 'MSI installer'
    $msiSignature = Get-SingleReleaseFile -Directory $msiDirectory -Filter "$($msi.Name).sig" -Description 'MSI updater signature'

    $nsisAssetName = "ihub_${version}_windows_x64_setup.exe"
    $msiAssetName = "ihub_${version}_windows_x64.msi"
    Copy-Item -LiteralPath $nsis.FullName -Destination (Join-Path $assetDirectory $nsisAssetName)
    Copy-Item -LiteralPath $nsisSignature.FullName -Destination (Join-Path $assetDirectory "$nsisAssetName.sig")
    Copy-Item -LiteralPath $msi.FullName -Destination (Join-Path $assetDirectory $msiAssetName)
    Copy-Item -LiteralPath $msiSignature.FullName -Destination (Join-Path $assetDirectory "$msiAssetName.sig")

    $signature = [IO.File]::ReadAllText((Join-Path $assetDirectory "$nsisAssetName.sig")).Trim()
    if ([string]::IsNullOrWhiteSpace($signature)) {
        throw 'The generated NSIS updater signature is empty.'
    }
    $downloadUrl = "https://github.com/$Repository/releases/download/$([Uri]::EscapeDataString($Tag))/$([Uri]::EscapeDataString($nsisAssetName))"
    $latest = [ordered]@{
        version = $version
        notes = "iHub $Tag stable Windows x64 release."
        pub_date = [DateTime]::UtcNow.ToString('o')
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                signature = $signature
                url = $downloadUrl
            }
        }
    }
    Write-Utf8WithoutBom -Path (Join-Path $assetDirectory 'latest.json') -Content ($latest | ConvertTo-Json -Depth 8)
    Invoke-External -Executable 'powershell' -CommandArguments @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts/generate-checksums.ps1', '-InputDirectory', $assetDirectory)
    Invoke-External -Executable 'node' -CommandArguments @('scripts/verify-release-assets.mjs', '--input-dir', $assetDirectory, '--repository', $Repository, '--tag', $Tag, '--platforms', 'windows-x86_64')

    if ([string]::IsNullOrWhiteSpace($NotesFile)) {
        $NotesFile = Join-Path $releaseRoot 'RELEASE_NOTES.md'
        $notes = @"
iHub $Tag 是 Windows 10/11 x64 稳定版。

- 支持导入和运行兼容的 uTools 插件。
- 支持从 GitHub 仓库或本地目录导入 iHub / uTools 插件。
- JSON 编辑器、本地搜索、取色器与插件中心已按桌面工作流整合。
- 启动器顶部支持直接按住移动；触摸与笔输入长按后拖动。

安装器已由本机打包并附 Tauri updater 签名与 SHA256SUMS.txt。`.sig` 不是 Windows Authenticode 发布者签名。
"@
        Write-Utf8WithoutBom -Path $NotesFile -Content $notes
    }
    else {
        $NotesFile = [IO.Path]::GetFullPath($NotesFile)
        if (-not (Test-Path -LiteralPath $NotesFile -PathType Leaf)) {
            throw "Release notes file not found: $NotesFile"
        }
    }

    if (-not $tagExists) {
        Invoke-External -Executable 'git' -CommandArguments @('tag', '-a', $Tag, '-m', "iHub $Tag")
        Invoke-External -Executable 'git' -CommandArguments @('push', 'origin', "refs/tags/$Tag")
        $tagExists = $true
    }

    if ($matchingReleases.Count -eq 0) {
        Invoke-External -Executable 'gh' -CommandArguments @('release', 'create', $Tag, '--repo', $Repository, '--verify-tag', '--draft', '--title', "iHub $Tag", '--notes-file', $NotesFile)
    }

    $assetPaths = @(
        Get-ChildItem -LiteralPath $assetDirectory -File |
            Sort-Object Name |
            ForEach-Object FullName
    )
    Invoke-External -Executable 'gh' -CommandArguments (@('release', 'upload', $Tag, '--repo', $Repository, '--clobber') + $assetPaths)

    [IO.Directory]::CreateDirectory($downloadDirectory) | Out-Null
    Invoke-External -Executable 'gh' -CommandArguments @('release', 'download', $Tag, '--repo', $Repository, '--dir', $downloadDirectory, '--clobber')
    Assert-DownloadedAssets -LocalDirectory $assetDirectory -DownloadedDirectory $downloadDirectory
    Invoke-External -Executable 'node' -CommandArguments @('scripts/verify-release-assets.mjs', '--input-dir', $downloadDirectory, '--repository', $Repository, '--tag', $Tag, '--platforms', 'windows-x86_64')

    if ($DraftOnly) {
        Write-Host "Draft $Tag uploaded and verified. It remains unpublished by request."
        exit 0
    }

    Invoke-External -Executable 'gh' -CommandArguments @('release', 'edit', $Tag, '--repo', $Repository, '--draft=false', '--prerelease=false', '--latest', '--title', "iHub $Tag", '--notes-file', $NotesFile)
    $published = Invoke-ExternalCapture -Executable 'gh' -CommandArguments @('release', 'view', $Tag, '--repo', $Repository, '--json', 'tagName,isDraft,isPrerelease,url,assets') | ConvertFrom-Json
    if ($published.tagName -ne $Tag -or $published.isDraft -eq $true -or $published.isPrerelease -eq $true) {
        throw "GitHub Release $Tag did not become a stable public release."
    }
    if (@($published.assets).Count -ne $assetPaths.Count) {
        throw "Published release asset count differs: expected=$($assetPaths.Count), actual=$(@($published.assets).Count)."
    }

    Write-Host "Published and verified ${Tag}: $($published.url)"
}
finally {
    Set-Location -LiteralPath $previousLocation
}
