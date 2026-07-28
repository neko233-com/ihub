# Starts iHub directly from this checkout. It intentionally never changes Git
# state unless an explicitly requested safe-update mode is used.

[CmdletBinding()]
param(
    [switch]$Update,

    [switch]$UpdateIfClean,

    [switch]$SkipInstall,

    [switch]$SkipCheck,

    [switch]$Build,

    [switch]$Package,

    [switch]$InstallLatest,

    [switch]$WatchInstall,

    [ValidateRange(1, 300)]
    [int]$WatchIntervalSeconds = 2,

    # An optional, literal file path used only for cooperative WatchInstall
    # shutdown. A present file makes the watcher exit at a safe boundary; it
    # never asks Windows to terminate another process.
    [AllowEmptyString()]
    [string]$WatchStopSignalPath,

    # Used by the trusted persistent-development wrapper to expose the actual
    # installed binary fingerprint and the latest install outcome. Normal
    # interactive WatchInstall runs do not need to set this internal path.
    [AllowEmptyString()]
    [string]$WatchStatusPath,

    [switch]$VerifyOnly,

    [switch]$Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Require-Command {
    param([Parameter(Mandatory)][string]$Name)

    if ($null -eq (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found on PATH."
    }
}

function Show-Usage {
    @'
Usage: .\scripts\dev.ps1 [options]

  -Update        Fetch and fast-forward only when the worktree is clean.
  -UpdateIfClean Attempt the same safe fast-forward before launch, but keep
                 running the current saved source when Git cannot update it.
  -SkipInstall   Do not run pnpm install --frozen-lockfile.
  -SkipCheck     Do not run pnpm check before the selected action.
  -Build         Build the current source without an installer bundle.
  -Package       Build signed native installer artifacts from the current source.
  -InstallLatest Build, validate, and silently install this exact current
                 worktree's signed NSIS package for the current Windows user.
  -WatchInstall  Explicitly watch saved, Git-visible source files and keep the
                 current local worktree installed after stable changes. It
                 never updates Git, launches iHub, or stops a process.
  -WatchIntervalSeconds  Poll interval for -WatchInstall (1-300; default: 2).
  -WatchStopSignalPath   Optional literal file path. When that file exists,
                 -WatchInstall exits cooperatively at its next safe boundary.
  -VerifyOnly    Verify prerequisites/dependencies without launching iHub.
  -Help          Show this help.

The normal launcher always uses the current worktree. -Update is strict;
-UpdateIfClean makes the development launcher follow upstream whenever that is
safe, while never overwriting uncommitted work. -InstallLatest never updates
Git and refuses to replace a running installed iHub process. -WatchInstall is
an explicit local-only mode; stop it with Ctrl+C when you no longer want builds
and installs to follow saved source changes.
'@ | Write-Host
}

function Invoke-External {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [string[]]$CommandArguments = @()
    )

    # A scheduled task has no visible console. Preserve a bounded tail of
    # native output in the watcher status when a command fails so a persistent
    # install cannot become an opaque "exit 1" loop. Output is captured only
    # for the explicit background watcher; normal interactive commands keep
    # their live streaming behavior.
    if ($WatchInstall -and -not [string]::IsNullOrWhiteSpace($WatchStatusPath)) {
        # Windows PowerShell wraps redirected native stderr lines as
        # ErrorRecord objects. With the script's fail-fast preference those
        # informational lines (for example pnpm's `$ tsc -b`) would otherwise
        # terminate the watcher before the native exit code can be inspected.
        $recentLines = [Collections.Generic.Queue[string]]::new()
        $previousErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            & $Executable @CommandArguments 2>&1 | ForEach-Object {
                $line = [string]$_
                Write-Host $line
                $trimmedLine = $line.Trim()
                if (-not [string]::IsNullOrWhiteSpace($trimmedLine)) {
                    # Bound both dimensions of the in-memory diagnostic tail:
                    # at most 24 lines and at most 1,000 retained characters
                    # from any one native output record.
                    if ($trimmedLine.Length -gt 1000) {
                        $trimmedLine = $trimmedLine.Substring($trimmedLine.Length - 1000)
                    }
                    $recentLines.Enqueue($trimmedLine)
                    while ($recentLines.Count -gt 24) {
                        [void]$recentLines.Dequeue()
                    }
                }
            }
            $exitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        if ($exitCode -ne 0) {
            $recentOutput = (@($recentLines.ToArray()) -join ' | ')
            if ($recentOutput.Length -gt 3500) {
                $recentOutput = $recentOutput.Substring($recentOutput.Length - 3500)
            }
            $suffix = if ([string]::IsNullOrWhiteSpace($recentOutput)) {
                ''
            }
            else {
                " Recent output: $recentOutput"
            }
            throw "Command failed (${exitCode}): $Executable $($CommandArguments -join ' ').$suffix"
        }
        return
    }

    & $Executable @CommandArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed ($LASTEXITCODE): $Executable $($CommandArguments -join ' ')"
    }
}

function Invoke-Pnpm {
    param([Parameter(Mandatory)][string[]]$PnpmArguments)

    Invoke-External -Executable 'corepack' -CommandArguments (@('pnpm') + $PnpmArguments)
}

function Get-GitOutput {
    param([Parameter(Mandatory)][string[]]$GitArguments)

    $output = & git @GitArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Git command failed ($LASTEXITCODE): git $($GitArguments -join ' ')"
    }

    return ($output -join "`n").Trim()
}

function Invoke-SafeFastForward {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string]$WorkTreeChanges,
        [switch]$ContinueOnSkip
    )

    if (-not [string]::IsNullOrWhiteSpace($WorkTreeChanges)) {
        $message = 'The worktree is dirty. No fetch, merge, reset, checkout, or clean operation was performed.'
        if ($ContinueOnSkip) {
            Write-Warning "Safe update skipped: $message"
            Write-Warning 'Continuing with the current saved source.'
            return
        }
        throw "-Update refuses to change a dirty worktree. Commit, stash, or inspect changes first. No source files were changed."
    }

    try {
        $upstreamOutput = & git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>$null
        if ($LASTEXITCODE -ne 0) {
            throw 'The current branch has no upstream.'
        }
        $upstream = ($upstreamOutput -join "`n").Trim()
        if ([string]::IsNullOrWhiteSpace($upstream)) {
            throw 'The current branch has no upstream.'
        }

        Invoke-External -Executable 'git' -CommandArguments @('fetch', '--prune')

        $counts = Get-GitOutput -GitArguments @('rev-list', '--left-right', '--count', "HEAD...$upstream")
        $countParts = @($counts -split '\s+' | Where-Object { $_ })
        if ($countParts.Count -ne 2 -or $countParts[0] -notmatch '^\d+$' -or $countParts[1] -notmatch '^\d+$') {
            throw "Could not determine divergence from $upstream."
        }

        $ahead = [int]$countParts[0]
        $behind = [int]$countParts[1]
        if ($ahead -gt 0 -and $behind -gt 0) {
            throw "Local branch has diverged from $upstream ($ahead ahead, $behind behind)."
        }
        if ($ahead -gt 0) {
            throw "Local branch is $ahead commit(s) ahead of $upstream."
        }
        if ($behind -gt 0) {
            Write-Host "Fast-forwarding $behind commit(s) from $upstream..."
            Invoke-External -Executable 'git' -CommandArguments @('merge', '--ff-only', $upstream)
        }
        else {
            Write-Host "Source is already current with $upstream."
        }
    }
    catch {
        if ($ContinueOnSkip) {
            Write-Warning "Safe update skipped: $($_.Exception.Message) No working-tree files were changed by iHub."
            Write-Warning 'Continuing with the current saved source.'
            return
        }
        throw
    }
}

function Invoke-SignedPackage {
    $hasProcessSigningKey = -not [string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY)
    if ($hasProcessSigningKey) {
        # This path only consumes the NSIS installer and its updater signature.
        # Avoid building unrelated Windows bundle formats for InstallLatest.
        Invoke-Pnpm -PnpmArguments @('tauri', 'build', '--bundles', 'nsis')
        return
    }

    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'A signed package needs TAURI_SIGNING_PRIVATE_KEY, or LOCALAPPDATA for iHub user-local signing material.'
    }

    $keyPath = if ([string]::IsNullOrWhiteSpace($env:IHUB_UPDATER_PRIVATE_KEY_PATH)) {
        Join-Path $env:LOCALAPPDATA 'iHub\keys\tauri-updater-release-v2.key'
    }
    else {
        [IO.Path]::GetFullPath($env:IHUB_UPDATER_PRIVATE_KEY_PATH)
    }
    $passwordPath = if ([string]::IsNullOrWhiteSpace($env:IHUB_UPDATER_PASSWORD_PATH)) {
        Join-Path $env:LOCALAPPDATA 'iHub\keys\tauri-updater-release-v2.password'
    }
    else {
        [IO.Path]::GetFullPath($env:IHUB_UPDATER_PASSWORD_PATH)
    }

    if (-not (Test-Path -LiteralPath $keyPath -PathType Leaf)) {
        throw "A signed package needs an updater private key. Missing: $keyPath. Set TAURI_SIGNING_PRIVATE_KEY for CI, or IHUB_UPDATER_PRIVATE_KEY_PATH for a local key file."
    }
    if (-not [string]::IsNullOrWhiteSpace($env:IHUB_UPDATER_PASSWORD_PATH) -and -not (Test-Path -LiteralPath $passwordPath -PathType Leaf)) {
        throw "IHUB_UPDATER_PASSWORD_PATH was set but does not point to a password file: $passwordPath"
    }

    $previousSigningKey = [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY', 'Process')
    $previousSigningPassword = [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PASSWORD', 'Process')
    try {
        # Tauri accepts a private-key file path as well as key contents. Keeping
        # the key on disk avoids loading it into command output or source files.
        $env:TAURI_SIGNING_PRIVATE_KEY = $keyPath
        if (Test-Path -LiteralPath $passwordPath -PathType Leaf) {
            # A password is optional: Tauri-generated keys may be unencrypted.
            $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [IO.File]::ReadAllText($passwordPath)
        }
        else {
            Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
        }
        Invoke-Pnpm -PnpmArguments @('tauri', 'build', '--bundles', 'nsis')
    }
    finally {
        if ($null -eq $previousSigningKey) {
            Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
        }
        else {
            $env:TAURI_SIGNING_PRIVATE_KEY = $previousSigningKey
        }
        if ($null -eq $previousSigningPassword) {
            Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
        }
        else {
            $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $previousSigningPassword
        }
    }
}

function Assert-PathIsChildOf {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Description
    )

    $normalizedRoot = [IO.Path]::GetFullPath($Root)
    $normalizedPath = [IO.Path]::GetFullPath($Path)
    $rootPrefix = if ($normalizedRoot.EndsWith([string][IO.Path]::DirectorySeparatorChar)) {
        $normalizedRoot
    }
    else {
        "$normalizedRoot$([IO.Path]::DirectorySeparatorChar)"
    }

    if (-not $normalizedPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description must stay below '$normalizedRoot': $normalizedPath"
    }

    return $normalizedPath
}

function Assert-NoReparsePointsBelowRoot {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Description
    )

    $normalizedRoot = [IO.Path]::GetFullPath($Root)
    $normalizedPath = Assert-PathIsChildOf -Path $Path -Root $normalizedRoot -Description $Description
    $relativePath = $normalizedPath.Substring($normalizedRoot.Length).TrimStart(
        [char[]]@([char]92, [char]47)
    )
    $candidate = $normalizedRoot
    foreach ($component in @($relativePath -split '[\\/]' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) {
        $candidate = Join-Path $candidate $component
        if (-not (Test-Path -LiteralPath $candidate)) {
            continue
        }

        $item = Get-Item -LiteralPath $candidate -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "$Description refuses a reparse-point path component: $($item.FullName)"
        }
    }
}

function Find-NestedGitWorktreeRoot {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][string]$Path
    )

    $normalizedRoot = [IO.Path]::GetFullPath($RepositoryRoot)
    $candidate = if ([IO.File]::Exists($Path)) {
        [IO.Path]::GetDirectoryName($Path)
    }
    elseif ([IO.Directory]::Exists($Path)) {
        [IO.Path]::GetFullPath($Path)
    }
    else {
        [IO.Path]::GetDirectoryName($Path)
    }

    while (-not [string]::IsNullOrWhiteSpace($candidate)) {
        if (-not $candidate.Equals($normalizedRoot, [StringComparison]::OrdinalIgnoreCase)) {
            # A worktree can represent its Git metadata as either a directory
            # or a gitdir pointer file. Both forms define an independent
            # repository boundary from the desktop shell's point of view.
            if (Test-Path -LiteralPath (Join-Path $candidate '.git')) {
                return $candidate
            }
        }

        if ($candidate.Equals($normalizedRoot, [StringComparison]::OrdinalIgnoreCase)) {
            break
        }
        $parent = [IO.Directory]::GetParent($candidate)
        if ($null -eq $parent) {
            break
        }
        $candidate = $parent.FullName
    }

    return $null
}

function Get-DevelopmentSourceFingerprint {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # Build output, dependencies, and .git internals are ignored by Git, so
    # they cannot cause the watcher to rebuild itself. The fingerprint covers
    # both tracked files and non-ignored developer files in the desktop shell,
    # including a deletion that Git still reports from the index. Independent
    # nested plugin worktrees are represented by one stable boundary instead:
    # their built assets are not bundled into the desktop app and should not
    # trigger an unrelated local app reinstall. It intentionally uses cheap
    # file metadata rather than reading every file's contents: this is a
    # debounce trigger for an explicitly requested local build, not an
    # integrity check.
    $paths = & git -C $RepositoryRoot ls-files --cached --others --exclude-standard
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not enumerate the saved iHub source files for WatchInstall.'
    }

    $input = New-Object System.Text.StringBuilder
    [void]$input.Append("ihub-development-watch-v1`n")
    $nestedWorktreeBoundaries = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    foreach ($relativePath in @($paths | Sort-Object)) {
        if ([string]::IsNullOrWhiteSpace($relativePath)) {
            continue
        }
        $fullPath = Assert-PathIsChildOf -Path (Join-Path $RepositoryRoot $relativePath) -Root $RepositoryRoot -Description 'Watched source path'
        $nestedWorktreeRoot = Find-NestedGitWorktreeRoot -RepositoryRoot $RepositoryRoot -Path $fullPath
        if ($null -ne $nestedWorktreeRoot) {
            $boundaryPath = (
                $nestedWorktreeRoot.Substring($RepositoryRoot.Length).TrimStart(
                    [char[]]@([char]92, [char]47)
                ).Replace([string][char]92, '/')
            )
            if ($nestedWorktreeBoundaries.Add($boundaryPath)) {
                [void]$input.Append($boundaryPath)
                [void]$input.Append([char]0)
                [void]$input.Append("nested-git-worktree-boundary`n")
            }
            continue
        }
        if ([IO.File]::Exists($fullPath)) {
            $item = [IO.FileInfo]::new($fullPath)
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "WatchInstall refuses to inspect a source file behind a reparse point: $fullPath"
            }
            [void]$input.Append($relativePath)
            [void]$input.Append([char]0)
            [void]$input.Append($item.Length)
            [void]$input.Append(':')
            [void]$input.Append($item.LastWriteTimeUtc.Ticks)
            [void]$input.Append("`n")
        }
        elseif ([IO.Directory]::Exists($fullPath)) {
            # Independent official plugins are nested Git worktrees. They are
            # not bundled into the root desktop app, so keep a stable boundary
            # marker instead of recursively walking another repository (or its
            # dependencies) on every poll.
            $directoryItem = [IO.DirectoryInfo]::new($fullPath)
            if (($directoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "WatchInstall refuses to traverse a source directory behind a reparse point: $fullPath"
            }
            [void]$input.Append($relativePath)
            [void]$input.Append([char]0)
            [void]$input.Append("directory-boundary`n")
        }
        elseif (Test-Path -LiteralPath $fullPath) {
            throw "WatchInstall only accepts regular source files or directories: $fullPath"
        }
        else {
            # A tracked file may have been deleted. Retaining the path in the
            # snapshot makes that deletion a rebuild-worthy source change.
            [void]$input.Append($relativePath)
            [void]$input.Append([char]0)
            [void]$input.Append("missing`n")
        }
    }

    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($input.ToString())
        return ([BitConverter]::ToString($sha256.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }
}

function Get-CurrentNsisPackageDescriptor {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    $configPath = Join-Path $RepositoryRoot 'src-tauri\tauri.conf.json'
    try {
        $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
    }
    catch {
        throw "Could not parse Tauri configuration '$configPath': $($_.Exception.Message)"
    }

    $productName = [string]$config.productName
    $version = [string]$config.version
    $binaryName = [string]$config.mainBinaryName
    if ($productName -notmatch '^[A-Za-z0-9][A-Za-z0-9 ._-]*$') {
        throw "Tauri productName is not safe for a local installer path: '$productName'."
    }
    if ($version -notmatch '^[0-9A-Za-z][0-9A-Za-z.+-]*$') {
        throw "Tauri version is not safe for an NSIS installer name: '$version'."
    }
    if ($binaryName -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$') {
        throw "Tauri mainBinaryName is not safe for an installed executable path: '$binaryName'."
    }

    $releaseRoot = Assert-PathIsChildOf -Path (Join-Path $RepositoryRoot 'src-tauri\target\release') -Root $RepositoryRoot -Description 'Tauri release directory'
    $releaseExecutablePath = Assert-PathIsChildOf -Path (Join-Path $releaseRoot ("$binaryName.exe")) -Root $releaseRoot -Description 'Tauri release executable path'
    $bundleRoot = Assert-PathIsChildOf -Path (Join-Path $releaseRoot 'bundle') -Root $releaseRoot -Description 'Tauri bundle directory'
    $nsisRoot = Assert-PathIsChildOf -Path (Join-Path $bundleRoot 'nsis') -Root $bundleRoot -Description 'NSIS bundle directory'
    $nsisBuildRoot = Assert-PathIsChildOf -Path (Join-Path $releaseRoot 'nsis\x64') -Root $releaseRoot -Description 'NSIS generated build directory'
    Assert-NoReparsePointsBelowRoot -Path $nsisRoot -Root $RepositoryRoot -Description 'Tauri NSIS bundle path'
    Assert-NoReparsePointsBelowRoot -Path $nsisBuildRoot -Root $RepositoryRoot -Description 'Tauri NSIS generated build path'
    $installerName = "${productName}_${version}_x64-setup.exe"
    if ([IO.Path]::GetFileName($installerName) -ne $installerName) {
        throw "Computed NSIS installer name is unsafe: '$installerName'."
    }
    $installerPath = Assert-PathIsChildOf -Path (Join-Path $nsisRoot $installerName) -Root $nsisRoot -Description 'NSIS installer path'
    $signaturePath = Assert-PathIsChildOf -Path "$installerPath.sig" -Root $nsisRoot -Description 'NSIS updater signature path'
    # Tauri's generated NSIS template uses `File "${MAINBINARYSRCPATH}"`
    # without `/oname`, so the immutable snapshot must retain the configured
    # main-binary file name. Otherwise NSIS installs the snapshot beside the
    # old application executable instead of replacing it.
    $payloadSnapshotPath = Assert-PathIsChildOf -Path (Join-Path $nsisBuildRoot ("$binaryName.exe")) -Root $nsisBuildRoot -Description 'NSIS payload snapshot path'
    $payloadProofPath = Assert-PathIsChildOf -Path (Join-Path $nsisBuildRoot 'nsis-output.exe.ihub-payload-proof.json') -Root $nsisBuildRoot -Description 'NSIS payload proof path'
    $payloadIncludePath = Assert-PathIsChildOf -Path (Join-Path $nsisBuildRoot 'nsis-output.exe.ihub-payload-proof.nsh') -Root $nsisBuildRoot -Description 'NSIS payload proof include path'

    return [pscustomobject]@{
        ProductName          = $productName
        Version              = $version
        BinaryName           = $binaryName
        RepositoryRoot       = [IO.Path]::GetFullPath($RepositoryRoot)
        ReleaseRoot          = $releaseRoot
        ReleaseExecutablePath = $releaseExecutablePath
        BundleRoot           = $bundleRoot
        NsisRoot             = $nsisRoot
        NsisBuildRoot        = $nsisBuildRoot
        InstallerPath        = $installerPath
        SignaturePath        = $signaturePath
        PayloadSnapshotPath  = $payloadSnapshotPath
        PayloadProofPath     = $payloadProofPath
        PayloadIncludePath   = $payloadIncludePath
    }
}

function Get-TrustedRegularFileFingerprint {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Description,
        [switch]$AllowMissing
    )

    $normalizedPath = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $normalizedPath)) {
        if ($AllowMissing) {
            return $null
        }
        throw "$Description is missing: $normalizedPath"
    }
    if (-not (Test-Path -LiteralPath $normalizedPath -PathType Leaf)) {
        throw "$Description is not a regular file: $normalizedPath"
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

function Get-TrustedNsisPayloadProof {
    param(
        [Parameter(Mandatory)]$Descriptor,
        [Parameter(Mandatory)][DateTime]$NotBefore
    )

    Assert-NoReparsePointsBelowRoot -Path $Descriptor.NsisBuildRoot -Root $Descriptor.RepositoryRoot -Description 'NSIS payload proof path'
    $proofFingerprint = Get-TrustedRegularFileFingerprint -Path $Descriptor.PayloadProofPath -Description 'makensis payload proof'
    if ($proofFingerprint.Length -gt 65536) {
        throw "The makensis payload proof is too large to trust: $($proofFingerprint.Path)"
    }

    try {
        $proof = Get-Content -LiteralPath $proofFingerprint.Path -Raw | ConvertFrom-Json
    }
    catch {
        throw "The makensis payload proof is not valid JSON: $($proofFingerprint.Path). $($_.Exception.Message)"
    }

    if ([string]$proof.managedBy -ne 'iHub NSIS payload proof v1' -or [string]$proof.schemaVersion -ne '1') {
        throw "The makensis payload proof has no trusted iHub schema marker: $($proofFingerprint.Path)"
    }
    $payloadSha256 = ([string]$proof.payloadSha256).ToLowerInvariant()
    if ($payloadSha256 -notmatch '^[0-9a-f]{64}$') {
        throw "The makensis payload proof contains an invalid SHA-256: $($proofFingerprint.Path)"
    }
    $nonce = ([string]$proof.nonce).ToLowerInvariant()
    if ($nonce -notmatch '^[0-9a-f]{32}$') {
        throw "The makensis payload proof contains an invalid build nonce: $($proofFingerprint.Path)"
    }
    [int64]$payloadLength = 0
    if (-not [int64]::TryParse([string]$proof.payloadLength, [ref]$payloadLength) -or $payloadLength -le 0) {
        throw "The makensis payload proof contains an invalid payload length: $($proofFingerprint.Path)"
    }
    $expectedSnapshotFileName = [IO.Path]::GetFileName($Descriptor.PayloadSnapshotPath)
    if (-not [string]::Equals([string]$proof.snapshotFileName, $expectedSnapshotFileName, [StringComparison]::Ordinal)) {
        throw "The makensis payload proof names an unexpected snapshot: '$($proof.snapshotFileName)'."
    }
    [DateTime]$generatedAt = [DateTime]::MinValue
    if (-not [DateTime]::TryParse(
            [string]$proof.generatedAt,
            [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::RoundtripKind,
            [ref]$generatedAt
        )) {
        throw "The makensis payload proof has an invalid generation time: $($proofFingerprint.Path)"
    }

    $snapshotFingerprint = Get-TrustedRegularFileFingerprint -Path $Descriptor.PayloadSnapshotPath -Description 'immutable makensis payload snapshot'
    if (
        -not [string]::Equals($snapshotFingerprint.Sha256, $payloadSha256, [StringComparison]::OrdinalIgnoreCase) -or
        $snapshotFingerprint.Length -ne $payloadLength
    ) {
        throw "The immutable makensis payload snapshot does not match its proof: $($snapshotFingerprint.Path)"
    }
    $freshnessFloor = $NotBefore.AddSeconds(-5)
    if (
        $proofFingerprint.LastWriteUtc -lt $freshnessFloor -or
        $snapshotFingerprint.LastWriteUtc -lt $freshnessFloor -or
        $generatedAt.ToUniversalTime() -lt $freshnessFloor
    ) {
        throw 'The makensis payload proof or immutable snapshot predates this packaging run.'
    }

    return [pscustomobject]@{
        Path                = $proofFingerprint.Path
        Fingerprint         = $proofFingerprint
        SnapshotFingerprint = $snapshotFingerprint
        PayloadSha256       = $payloadSha256
        PayloadLength       = $payloadLength
        Nonce               = $nonce
        GeneratedAt         = $generatedAt.ToUniversalTime()
    }
}

function Get-NsisPackageState {
    param([Parameter(Mandatory)]$Descriptor)

    Assert-NoReparsePointsBelowRoot -Path $Descriptor.NsisRoot -Root $Descriptor.RepositoryRoot -Description 'NSIS package path'
    Assert-NoReparsePointsBelowRoot -Path $Descriptor.NsisBuildRoot -Root $Descriptor.RepositoryRoot -Description 'NSIS payload proof path'
    if (
        -not (Test-Path -LiteralPath $Descriptor.InstallerPath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $Descriptor.SignaturePath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $Descriptor.PayloadProofPath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $Descriptor.PayloadSnapshotPath -PathType Leaf)
    ) {
        return $null
    }

    $installer = Get-Item -LiteralPath $Descriptor.InstallerPath -Force
    $signature = Get-Item -LiteralPath $Descriptor.SignaturePath -Force
    $payloadProof = Get-Item -LiteralPath $Descriptor.PayloadProofPath -Force
    $payloadSnapshot = Get-Item -LiteralPath $Descriptor.PayloadSnapshotPath -Force
    foreach ($item in @($installer, $signature, $payloadProof, $payloadSnapshot)) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing an NSIS package artifact behind a reparse point: $($item.FullName)"
        }
        if ($item.Length -le 0) {
            throw "NSIS package artifact is empty: $($item.FullName)"
        }
    }

    return [pscustomobject]@{
        InstallerPath          = $installer.FullName
        SignaturePath          = $signature.FullName
        InstallerLength        = [int64]$installer.Length
        SignatureLength        = [int64]$signature.Length
        PayloadProofLength     = [int64]$payloadProof.Length
        PayloadSnapshotLength  = [int64]$payloadSnapshot.Length
        InstallerLastWriteUtc  = $installer.LastWriteTimeUtc
        SignatureLastWriteUtc  = $signature.LastWriteTimeUtc
        PayloadProofLastWriteUtc = $payloadProof.LastWriteTimeUtc
        PayloadSnapshotLastWriteUtc = $payloadSnapshot.LastWriteTimeUtc
        Fingerprint            = "$($installer.Length):$($installer.LastWriteTimeUtc.Ticks):$($signature.Length):$($signature.LastWriteTimeUtc.Ticks):$($payloadProof.Length):$($payloadProof.LastWriteTimeUtc.Ticks):$($payloadSnapshot.Length):$($payloadSnapshot.LastWriteTimeUtc.Ticks)"
    }
}

function Clear-ExpectedNsisArtifacts {
    param([Parameter(Mandatory)]$Descriptor)

    Assert-NoReparsePointsBelowRoot -Path $Descriptor.NsisRoot -Root $Descriptor.RepositoryRoot -Description 'NSIS artifact cleanup path'
    Assert-NoReparsePointsBelowRoot -Path $Descriptor.NsisBuildRoot -Root $Descriptor.RepositoryRoot -Description 'NSIS proof cleanup path'
    # A failed Tauri signing pass can leave a freshly bundled installer beside
    # an older .sig or proof. Only remove exact descriptor-derived paths before
    # a new build; never glob or recurse through either output directory.
    foreach ($artifact in @(
            [pscustomobject]@{ Name = 'NSIS installer'; Path = [string]$Descriptor.InstallerPath; Root = [string]$Descriptor.NsisRoot },
            [pscustomobject]@{ Name = 'NSIS updater signature'; Path = [string]$Descriptor.SignaturePath; Root = [string]$Descriptor.NsisRoot },
            [pscustomobject]@{ Name = 'NSIS payload snapshot'; Path = [string]$Descriptor.PayloadSnapshotPath; Root = [string]$Descriptor.NsisBuildRoot },
            [pscustomobject]@{ Name = 'NSIS payload proof'; Path = [string]$Descriptor.PayloadProofPath; Root = [string]$Descriptor.NsisBuildRoot },
            [pscustomobject]@{ Name = 'NSIS payload proof include'; Path = [string]$Descriptor.PayloadIncludePath; Root = [string]$Descriptor.NsisBuildRoot }
        )) {
        $artifactPath = Assert-PathIsChildOf -Path $artifact.Path -Root $artifact.Root -Description $artifact.Name
        if (-not (Test-Path -LiteralPath $artifactPath)) {
            continue
        }

        $item = Get-Item -LiteralPath $artifactPath -Force
        if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "Refusing to remove unsafe $($artifact.Name) artifact: $($item.FullName)"
        }

        Remove-Item -LiteralPath $item.FullName -Force
        if (Test-Path -LiteralPath $item.FullName) {
            throw "Could not clear previous $($artifact.Name) artifact: $($item.FullName)"
        }
    }
}

function Wait-ForCurrentNsisPackage {
    param(
        [Parameter(Mandatory)]$Descriptor,
        [Parameter(Mandatory)][DateTime]$NotBefore,
        [int]$TimeoutSeconds = 180
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $minimumWriteTime = $NotBefore
    $previousFingerprint = $null
    $stableObservations = 0

    while ([DateTime]::UtcNow -lt $deadline) {
        $state = Get-NsisPackageState -Descriptor $Descriptor
        if (
            $null -ne $state -and
            $state.InstallerLastWriteUtc -ge $minimumWriteTime -and
            $state.SignatureLastWriteUtc -ge $minimumWriteTime -and
            $state.PayloadProofLastWriteUtc -ge $minimumWriteTime.AddSeconds(-5) -and
            $state.PayloadSnapshotLastWriteUtc -ge $minimumWriteTime.AddSeconds(-5)
        ) {
            if ($state.SignatureLastWriteUtc.AddSeconds(2) -lt $state.InstallerLastWriteUtc) {
                # Tauri writes the updater signature as a separate artifact. Do
                # not install an installer while its matching sidecar is stale.
                $stableObservations = 0
                $previousFingerprint = $null
            }
            elseif ($state.Fingerprint -eq $previousFingerprint) {
                $stableObservations++
                if ($stableObservations -ge 2) {
                    return $state
                }
            }
            else {
                $previousFingerprint = $state.Fingerprint
                $stableObservations = 0
            }
        }
        else {
            $stableObservations = 0
            $previousFingerprint = $null
        }

        Start-Sleep -Milliseconds 750
    }

    throw "Timed out waiting for a fresh, stable signed NSIS package and payload proof. Expected '$($Descriptor.InstallerPath)', '$($Descriptor.SignaturePath)', and '$($Descriptor.PayloadProofPath)'. No installer was started."
}

function Build-CurrentSignedNsisPackage {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    $before = Get-CurrentNsisPackageDescriptor -RepositoryRoot $RepositoryRoot
    $releaseExecutableBefore = Get-TrustedRegularFileFingerprint -Path $before.ReleaseExecutablePath -Description 'Tauri release executable before packaging' -AllowMissing
    if ($null -eq $releaseExecutableBefore) {
        Write-Host "Release executable SHA-256 before packaging: <missing> ($($before.ReleaseExecutablePath))"
    }
    else {
        Write-Host "Release executable SHA-256 before packaging: $($releaseExecutableBefore.Sha256)"
    }
    Clear-ExpectedNsisArtifacts -Descriptor $before
    $packagingStartedAt = [DateTime]::UtcNow
    # Keep build logs visible, but do not allow their success-stream text to
    # become part of this function's object return value.
    Invoke-SignedPackage | Out-Host

    # Re-read the configuration so a concurrent source edit cannot make us
    # silently install a package addressed by an older config value.
    $after = Get-CurrentNsisPackageDescriptor -RepositoryRoot $RepositoryRoot
    if (
        -not [string]::Equals($before.InstallerPath, $after.InstallerPath, [StringComparison]::OrdinalIgnoreCase) -or
        -not [string]::Equals($before.SignaturePath, $after.SignaturePath, [StringComparison]::OrdinalIgnoreCase) -or
        -not [string]::Equals($before.PayloadProofPath, $after.PayloadProofPath, [StringComparison]::OrdinalIgnoreCase)
    ) {
        throw 'Tauri installer identity changed while packaging. No installer was started; rerun after the configuration is stable.'
    }

    $state = Wait-ForCurrentNsisPackage -Descriptor $after -NotBefore $packagingStartedAt
    $payloadProof = Get-TrustedNsisPayloadProof -Descriptor $after -NotBefore $packagingStartedAt
    $releaseExecutableAfter = Get-TrustedRegularFileFingerprint -Path $after.ReleaseExecutablePath -Description 'Tauri release executable restored after packaging'
    Write-Host "Restored unbundled executable SHA-256:       $($releaseExecutableAfter.Sha256)"
    Write-Host "Immutable makensis payload SHA-256:         $($payloadProof.PayloadSha256)"
    return [pscustomobject]@{
        Descriptor              = $after
        State                   = $state
        Sha256                  = (Get-FileHash -LiteralPath $state.InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
        PackagingStartedAt      = $packagingStartedAt
        ReleaseExecutableBefore = $releaseExecutableBefore
        ReleaseExecutableAfter  = $releaseExecutableAfter
        PayloadProof            = $payloadProof
        PayloadSha256           = $payloadProof.PayloadSha256
        PayloadLength           = $payloadProof.PayloadLength
        PayloadNonce            = $payloadProof.Nonce
    }
}

function Get-ExactInstalledTarget {
    param([Parameter(Mandatory)]$Descriptor)

    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'LOCALAPPDATA is unavailable; cannot validate the per-user iHub installation target.'
    }

    $localAppDataRoot = [IO.Path]::GetFullPath($env:LOCALAPPDATA)
    if (-not (Test-Path -LiteralPath $localAppDataRoot -PathType Container)) {
        throw "LOCALAPPDATA is not an available directory: $localAppDataRoot"
    }
    $localAppDataItem = Get-Item -LiteralPath $localAppDataRoot -Force
    if (($localAppDataItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to install through a reparse-point LOCALAPPDATA directory: $localAppDataRoot"
    }
    $installRoot = Assert-PathIsChildOf -Path (Join-Path $localAppDataRoot $Descriptor.ProductName) -Root $localAppDataRoot -Description 'Per-user iHub install directory'
    if (Test-Path -LiteralPath $installRoot) {
        $installRootItem = Get-Item -LiteralPath $installRoot -Force
        if (-not $installRootItem.PSIsContainer) {
            throw "The expected iHub install path is not a directory: $installRoot"
        }
        if (($installRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to install through a reparse-point iHub directory: $installRoot"
        }
    }

    $executablePath = Assert-PathIsChildOf -Path (Join-Path $installRoot ("$($Descriptor.BinaryName).exe")) -Root $installRoot -Description 'Installed iHub executable path'
    $requiredExecutablePath = Assert-PathIsChildOf -Path (Join-Path $localAppDataRoot 'iHub\ihub.exe') -Root $localAppDataRoot -Description 'Required iHub per-user executable path'
    if (-not [string]::Equals($executablePath, $requiredExecutablePath, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The development installer is pinned to '$requiredExecutablePath', but the current Tauri configuration resolves to '$executablePath'. No installer was started."
    }
    if (Test-Path -LiteralPath $executablePath) {
        $executableItem = Get-Item -LiteralPath $executablePath -Force
        if ($executableItem.PSIsContainer) {
            throw "The expected iHub executable path is a directory: $executablePath"
        }
        if (($executableItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to install through a reparse-point iHub executable: $executablePath"
        }
    }
    $proofPath = Assert-PathIsChildOf -Path (Join-Path $installRoot '.ihub-install-proof.json') -Root $installRoot -Description 'Installed iHub payload proof path'
    return [pscustomobject]@{
        InstallRoot    = $installRoot
        ExecutablePath = $executablePath
        ProofPath      = $proofPath
    }
}

function Get-DevelopmentPackageInstallMutexName {
    param([Parameter(Mandatory)]$Target)

    if ($null -eq $Target -or [string]::IsNullOrWhiteSpace([string]$Target.ExecutablePath)) {
        throw 'Cannot create the development package/install mutex without the exact installed iHub executable path.'
    }

    try {
        # Windows compares the target path without regard to case. Hashing its
        # normalized uppercase form gives every process that could replace the
        # same installed binary one stable, bounded named-mutex identity.
        $scopePath = [IO.Path]::GetFullPath([string]$Target.ExecutablePath).ToUpperInvariant()
    }
    catch {
        throw "Cannot normalize the development package/install target '$($Target.ExecutablePath)': $($_.Exception.Message)"
    }

    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($scopePath)
        $hash = ([BitConverter]::ToString($sha256.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }

    # Global prevents a second interactive Windows session from replacing the
    # same per-user install concurrently. The target path is already under
    # LOCALAPPDATA, and hashing means no file path is leaked into the namespace.
    return "Global\iHub-DevelopmentPackageInstall-$hash"
}

function Test-IHubExceptionMarker {
    param(
        [Parameter(Mandatory)]$ErrorRecord,
        [Parameter(Mandatory)][string]$Marker
    )

    $exception = $ErrorRecord.Exception
    while ($null -ne $exception) {
        if ($null -ne $exception.Data -and $exception.Data.Contains($Marker) -and [bool]$exception.Data[$Marker]) {
            return $true
        }
        $exception = $exception.InnerException
    }

    return $false
}

function Test-WatchInstallStopSignal {
    param([AllowEmptyString()][string]$StopSignalPath)

    if ([string]::IsNullOrWhiteSpace($StopSignalPath)) {
        return $false
    }

    try {
        $normalizedPath = [IO.Path]::GetFullPath($StopSignalPath)
    }
    catch {
        throw "WatchInstall stop signal path is invalid: '$StopSignalPath'. $($_.Exception.Message)"
    }

    if (-not (Test-Path -LiteralPath $normalizedPath)) {
        return $false
    }

    try {
        $item = Get-Item -LiteralPath $normalizedPath -Force -ErrorAction Stop
    }
    catch [System.Management.Automation.ItemNotFoundException] {
        # The file was removed between Test-Path and Get-Item, so no shutdown
        # was requested at this safe boundary.
        return $false
    }

    if ($item.PSIsContainer) {
        throw "WatchInstall stop signal must be a regular file, not a directory: $normalizedPath"
    }
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "WatchInstall refuses a stop signal behind a reparse point: $normalizedPath"
    }

    return $true
}

function Stop-WatchInstallIfRequested {
    param([AllowEmptyString()][string]$StopSignalPath)

    if (-not (Test-WatchInstallStopSignal -StopSignalPath $StopSignalPath)) {
        return
    }

    $exception = [OperationCanceledException]::new("WatchInstall stop signal detected: $([IO.Path]::GetFullPath($StopSignalPath)). No iHub process was stopped.")
    $exception.Data['iHubWatchInstallStopSignal'] = $true
    throw $exception
}

function Resolve-DevelopmentInstallWatchStatusPath {
    param([AllowEmptyString()][string]$StatusPath)

    if ([string]::IsNullOrWhiteSpace($StatusPath)) {
        return $null
    }

    try {
        $normalizedPath = [IO.Path]::GetFullPath($StatusPath)
    }
    catch {
        throw "WatchInstall status path is invalid: '$StatusPath'. $($_.Exception.Message)"
    }

    $statusDirectory = [IO.Path]::GetDirectoryName($normalizedPath)
    if ([string]::IsNullOrWhiteSpace($statusDirectory) -or -not (Test-Path -LiteralPath $statusDirectory -PathType Container)) {
        throw "WatchInstall status directory is unavailable: $statusDirectory"
    }
    $directoryItem = Get-Item -LiteralPath $statusDirectory -Force
    if (($directoryItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "WatchInstall refuses to write status through a reparse-point directory: $statusDirectory"
    }

    if (Test-Path -LiteralPath $normalizedPath) {
        if (-not (Test-Path -LiteralPath $normalizedPath -PathType Leaf)) {
            throw "WatchInstall status path is not a regular file: $normalizedPath"
        }
        $statusItem = Get-Item -LiteralPath $normalizedPath -Force
        if ($statusItem.PSIsContainer -or (($statusItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "WatchInstall refuses to replace an unsafe status file: $normalizedPath"
        }
    }

    return $normalizedPath
}

function Get-DevelopmentInstallWatchStatusState {
    param([AllowEmptyString()][string]$StatusPath)

    $emptyState = [pscustomobject]@{
        InstalledFingerprint = $null
        LastSuccessAt        = $null
        LastError            = $null
    }
    $normalizedPath = Resolve-DevelopmentInstallWatchStatusPath -StatusPath $StatusPath
    if ($null -eq $normalizedPath -or -not (Test-Path -LiteralPath $normalizedPath -PathType Leaf)) {
        return $emptyState
    }

    $statusItem = Get-Item -LiteralPath $normalizedPath -Force
    if ($statusItem.Length -gt 65536) {
        throw "WatchInstall status file is too large to trust: $normalizedPath"
    }
    try {
        $status = Get-Content -LiteralPath $normalizedPath -Raw | ConvertFrom-Json
    }
    catch {
        throw "WatchInstall status file is invalid: $normalizedPath. $($_.Exception.Message)"
    }
    $managedByProperty = $status.PSObject.Properties['managedBy']
    $serviceProperty = $status.PSObject.Properties['service']
    if ($null -eq $managedByProperty -or $null -eq $serviceProperty -or $managedByProperty.Value -ne 'iHub Development persistent install service v1' -or $serviceProperty.Value -ne 'watch-install') {
        throw "WatchInstall status file has no trusted ownership marker: $normalizedPath"
    }

    $installedFingerprint = if ($null -eq $status.PSObject.Properties['installedFingerprint'] -or [string]::IsNullOrWhiteSpace([string]$status.installedFingerprint)) {
        $null
    }
    else {
        ([string]$status.installedFingerprint).ToLowerInvariant()
    }
    if ($null -ne $installedFingerprint -and $installedFingerprint -notmatch '^[0-9a-f]{64}$') {
        throw "WatchInstall status file contains an invalid installed fingerprint: $normalizedPath"
    }

    return [pscustomobject]@{
        InstalledFingerprint = $installedFingerprint
        LastSuccessAt        = if ($null -eq $status.PSObject.Properties['lastSuccessAt'] -or [string]::IsNullOrWhiteSpace([string]$status.lastSuccessAt)) { $null } else { [string]$status.lastSuccessAt }
        LastError            = if ($null -eq $status.PSObject.Properties['lastError'] -or [string]::IsNullOrWhiteSpace([string]$status.lastError)) { $null } else { [string]$status.lastError }
    }
}

function Write-DevelopmentInstallWatchStatus {
    param(
        [AllowEmptyString()][string]$StatusPath,
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][ValidatePattern('^[a-z][a-z0-9-]*$')][string]$State,
        [Parameter(Mandatory)][string]$Message,
        [AllowNull()]$InstalledFingerprint,
        [AllowNull()]$LastSuccessAt,
        [AllowNull()]$LastError
    )

    $normalizedPath = Resolve-DevelopmentInstallWatchStatusPath -StatusPath $StatusPath
    if ($null -eq $normalizedPath) {
        return
    }

    $normalizedInstalledFingerprint = if ([string]::IsNullOrWhiteSpace([string]$InstalledFingerprint)) {
        $null
    }
    else {
        ([string]$InstalledFingerprint).ToLowerInvariant()
    }
    if ($null -ne $normalizedInstalledFingerprint -and $normalizedInstalledFingerprint -notmatch '^[0-9a-f]{64}$') {
        throw "Refusing to write an invalid installed executable fingerprint to WatchInstall status: '$InstalledFingerprint'."
    }

    $normalizedLastError = if ([string]::IsNullOrWhiteSpace([string]$LastError)) {
        $null
    }
    else {
        [string]$LastError
    }
    if ($null -ne $normalizedLastError -and $normalizedLastError.Length -gt 8192) {
        $normalizedLastError = $normalizedLastError.Substring(0, 8192)
    }
    $normalizedMessage = if ($Message.Length -gt 8192) { $Message.Substring(0, 8192) } else { $Message }

    $payload = [ordered]@{
        schemaVersion        = 2
        managedBy            = 'iHub Development persistent install service v1'
        service              = 'watch-install'
        sourceRoot           = [IO.Path]::GetFullPath($RepositoryRoot)
        state                = $State
        message              = $normalizedMessage
        updatedAt            = [DateTime]::UtcNow.ToString('o')
        installedFingerprint = $normalizedInstalledFingerprint
        lastSuccessAt        = if ([string]::IsNullOrWhiteSpace([string]$LastSuccessAt)) { $null } else { [string]$LastSuccessAt }
        lastError            = $normalizedLastError
    }
    $statusDirectory = [IO.Path]::GetDirectoryName($normalizedPath)
    $temporaryPath = Join-Path $statusDirectory (".$([IO.Path]::GetFileName($normalizedPath)).$([guid]::NewGuid().ToString('N')).tmp")
    try {
        Set-Content -LiteralPath $temporaryPath -Value ($payload | ConvertTo-Json -Depth 5) -Encoding UTF8 -NoNewline
        Move-Item -LiteralPath $temporaryPath -Destination $normalizedPath -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporaryPath) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
}

function Invoke-WithDevelopmentPackageInstallMutex {
    param(
        [Parameter(Mandatory)]$Target,
        [Parameter(Mandatory)][scriptblock]$Action,
        [AllowEmptyString()]
        [string]$WatchStopSignalPath,
        [ValidateRange(1, 1800)]
        [int]$TimeoutSeconds = 300
    )

    $mutexName = Get-DevelopmentPackageInstallMutexName -Target $Target
    $mutex = $null
    $lockTaken = $false
    try {
        try {
            $mutex = [Threading.Mutex]::new($false, $mutexName)
        }
        catch {
            throw "Could not create the development package/install mutex '$mutexName': $($_.Exception.Message)"
        }

        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        while (-not $lockTaken -and [DateTime]::UtcNow -lt $deadline) {
            # A scheduled watcher can be disabled without waiting for another
            # build to finish. This is cooperative only: it never interrupts
            # the owner or invokes Stop-Process.
            Stop-WatchInstallIfRequested -StopSignalPath $WatchStopSignalPath
            $remaining = $deadline - [DateTime]::UtcNow
            if ($remaining -le [TimeSpan]::Zero) {
                break
            }
            $waitSlice = if ($remaining.TotalSeconds -gt 1) {
                [TimeSpan]::FromSeconds(1)
            }
            else {
                $remaining
            }

            try {
                $lockTaken = $mutex.WaitOne($waitSlice)
            }
            catch [Threading.AbandonedMutexException] {
                # Windows grants ownership to this waiter when the previous
                # owner dies. Clear-then-build below makes a fresh package
                # before any installer is launched, so continue while making
                # recovery clear.
                $lockTaken = $true
                Write-Warning "Recovered an abandoned development package/install mutex for '$($Target.ExecutablePath)'. A new package will be built before installation."
            }
            catch {
                throw "Could not acquire the development package/install mutex '$mutexName': $($_.Exception.Message)"
            }
        }

        if (-not $lockTaken) {
            $exception = [TimeoutException]::new("Timed out after $TimeoutSeconds second(s) waiting for another local development package/install operation targeting '$($Target.ExecutablePath)'. No NSIS artifact was cleared, built, or installed by this invocation.")
            $exception.Data['iHubDevelopmentPackageInstallMutexTimeout'] = $true
            $exception.Data['iHubDevelopmentPackageInstallMutexName'] = $mutexName
            throw $exception
        }

        return (& $Action)
    }
    finally {
        if ($lockTaken -and $null -ne $mutex) {
            try {
                [void]$mutex.ReleaseMutex()
            }
            catch {
                # Do not mask an action failure after the mutex has already
                # released or the PowerShell host is shutting down.
                Write-Warning "Could not explicitly release development package/install mutex '$mutexName': $($_.Exception.Message)"
            }
        }
        if ($null -ne $mutex) {
            try {
                $mutex.Dispose()
            }
            catch {
                Write-Warning "Could not dispose development package/install mutex '$mutexName': $($_.Exception.Message)"
            }
        }
    }
}

function Get-ExactInstalledIHubProcessState {
    param([Parameter(Mandatory)][string]$ExecutablePath)

    $expectedPath = [IO.Path]::GetFullPath($ExecutablePath)
    $exactMatches = @()
    $unknownPathPids = @()
    foreach ($process in @(Get-Process -Name 'ihub' -ErrorAction SilentlyContinue)) {
        try {
            $processPath = [string]$process.Path
        }
        catch {
            $unknownPathPids += $process.Id
            continue
        }

        if ([string]::IsNullOrWhiteSpace($processPath)) {
            $unknownPathPids += $process.Id
            continue
        }

        try {
            $normalizedProcessPath = [IO.Path]::GetFullPath($processPath)
        }
        catch {
            $unknownPathPids += $process.Id
            continue
        }

        if ([string]::Equals($normalizedProcessPath, $expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
            $exactMatches += $process.Id
        }
    }

    return [pscustomobject]@{
        ExpectedPath    = $expectedPath
        ExactMatches    = @($exactMatches)
        UnknownPathPids = @($unknownPathPids)
    }
}

function Assert-ExactInstalledExecutableIsNotRunning {
    param([Parameter(Mandatory)][string]$ExecutablePath)

    $state = Get-ExactInstalledIHubProcessState -ExecutablePath $ExecutablePath
    if ($state.ExactMatches.Count -gt 0) {
        throw "The exact installed iHub executable is running (PID $($state.ExactMatches -join ', ')): $($state.ExpectedPath). Close it yourself, then rerun -InstallLatest or keep -WatchInstall open. The script never stops processes."
    }
    if ($state.UnknownPathPids.Count -gt 0) {
        Write-Warning "Could not inspect iHub process path(s) for PID $($state.UnknownPathPids -join ', '). No process will be stopped; if NSIS reports a file in use, close iHub manually and retry."
    }
}

function Get-TrustedInstalledNsisPayloadProof {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][DateTime]$NotBefore
    )

    $normalizedPath = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $normalizedPath -PathType Leaf)) {
        throw "The installed NSIS payload proof marker is missing: $normalizedPath"
    }
    $item = Get-Item -LiteralPath $normalizedPath -Force
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "The installed NSIS payload proof marker is unsafe: $normalizedPath"
    }
    if ($item.Length -le 0 -or $item.Length -gt 65536) {
        throw "The installed NSIS payload proof marker has an invalid size: $normalizedPath"
    }
    if ($item.LastWriteTimeUtc -lt $NotBefore.AddSeconds(-5)) {
        throw "The installed NSIS payload proof marker predates this installer run: $normalizedPath"
    }

    try {
        $proof = Get-Content -LiteralPath $item.FullName -Raw | ConvertFrom-Json
    }
    catch {
        throw "The installed NSIS payload proof marker is not valid JSON: $normalizedPath. $($_.Exception.Message)"
    }
    if ([string]$proof.managedBy -ne 'iHub NSIS payload proof v1' -or [string]$proof.schemaVersion -ne '1') {
        throw "The installed NSIS payload proof marker has no trusted iHub schema marker: $normalizedPath"
    }

    $payloadSha256 = ([string]$proof.payloadSha256).ToLowerInvariant()
    if ($payloadSha256 -notmatch '^[0-9a-f]{64}$') {
        throw "The installed NSIS payload proof marker contains an invalid SHA-256: $normalizedPath"
    }
    $nonce = ([string]$proof.nonce).ToLowerInvariant()
    if ($nonce -notmatch '^[0-9a-f]{32}$') {
        throw "The installed NSIS payload proof marker contains an invalid build nonce: $normalizedPath"
    }
    [int64]$payloadLength = 0
    if (-not [int64]::TryParse([string]$proof.payloadLength, [ref]$payloadLength) -or $payloadLength -le 0) {
        throw "The installed NSIS payload proof marker contains an invalid payload length: $normalizedPath"
    }

    return [pscustomobject]@{
        Path             = $item.FullName
        LastWriteUtc     = $item.LastWriteTimeUtc
        PayloadSha256    = $payloadSha256
        PayloadLength    = $payloadLength
        Nonce            = $nonce
    }
}

function Clear-InstalledNsisPayloadProof {
    param([Parameter(Mandatory)]$Target)

    if (-not (Test-Path -LiteralPath $Target.ProofPath)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Target.ProofPath -PathType Leaf)) {
        throw "Refusing to replace a non-file installed payload proof path: $($Target.ProofPath)"
    }
    $item = Get-Item -LiteralPath $Target.ProofPath -Force
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) -or $item.Length -le 0 -or $item.Length -gt 65536) {
        throw "Refusing to replace an unsafe installed payload proof marker: $($Target.ProofPath)"
    }
    try {
        $existingProof = Get-Content -LiteralPath $item.FullName -Raw | ConvertFrom-Json
    }
    catch {
        throw "Refusing to remove an untrusted installed payload proof marker: $($Target.ProofPath)"
    }
    if ([string]$existingProof.managedBy -ne 'iHub NSIS payload proof v1' -or [string]$existingProof.schemaVersion -ne '1') {
        throw "Refusing to remove an installed payload proof marker without the iHub ownership schema: $($Target.ProofPath)"
    }

    Remove-Item -LiteralPath $item.FullName -Force
    if (Test-Path -LiteralPath $item.FullName) {
        throw "Could not clear the previous installed payload proof marker: $($Target.ProofPath)"
    }
}

function Install-CurrentNsisPackage {
    param(
        [Parameter(Mandatory)]$Package,
        [Parameter(Mandatory)]$Target
    )

    Assert-ExactInstalledExecutableIsNotRunning -ExecutablePath $Target.ExecutablePath

    Write-Host "Installing exact local NSIS package: $($Package.State.InstallerPath)"
    Write-Host "Package SHA-256: $($Package.Sha256)"
    Write-Host "Expected installed executable SHA-256: $($Package.PayloadSha256)"
    Write-Host "Validated per-user target: $($Target.ExecutablePath)"

    $currentPayloadProof = Get-TrustedNsisPayloadProof -Descriptor $Package.Descriptor -NotBefore $Package.PackagingStartedAt
    if (
        -not [string]::Equals($currentPayloadProof.PayloadSha256, [string]$Package.PayloadSha256, [StringComparison]::OrdinalIgnoreCase) -or
        $currentPayloadProof.PayloadLength -ne [int64]$Package.PayloadLength -or
        -not [string]::Equals($currentPayloadProof.Nonce, [string]$Package.PayloadNonce, [StringComparison]::Ordinal)
    ) {
        throw 'The makensis payload proof changed after packaging. No installer was started.'
    }
    Assert-NoReparsePointsBelowRoot -Path $Package.State.InstallerPath -Root $Package.Descriptor.RepositoryRoot -Description 'NSIS installer immediately before launch'
    $currentInstallerFingerprint = Get-TrustedRegularFileFingerprint -Path $Package.State.InstallerPath -Description 'NSIS installer immediately before launch'
    Assert-NoReparsePointsBelowRoot -Path $Package.State.InstallerPath -Root $Package.Descriptor.RepositoryRoot -Description 'NSIS installer after hashing'
    if (-not [string]::Equals($currentInstallerFingerprint.Sha256, [string]$Package.Sha256, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The NSIS installer changed after packaging. Expected SHA-256 '$($Package.Sha256)', found '$($currentInstallerFingerprint.Sha256)'. No installer was started."
    }
    if ([string]$Package.PayloadSha256 -notmatch '^[0-9a-f]{64}$') {
        throw 'The current package has no valid makensis-payload SHA-256. No installer was started.'
    }
    Clear-InstalledNsisPayloadProof -Target $Target

    $authenticode = Get-AuthenticodeSignature -LiteralPath $Package.State.InstallerPath
    if ($authenticode.Status -eq 'Valid') {
        Write-Host "Authenticode verification passed: $($authenticode.SignerCertificate.Subject)"
    }
    else {
        Write-Warning "Local installer Authenticode status is '$($authenticode.Status)'. Its required Tauri updater signature sidecar is present; configure Authenticode signing separately for a signed Windows publisher identity."
    }

    $installerStartedAt = [DateTime]::UtcNow
    $installerProcess = Start-Process -FilePath $Package.State.InstallerPath -ArgumentList @('/S') -Wait -PassThru
    if ($installerProcess.ExitCode -notin @(0, 3010)) {
        throw "The local NSIS installer exited with code $($installerProcess.ExitCode)."
    }
    if ($installerProcess.ExitCode -eq 3010) {
        Write-Warning 'iHub installed successfully; Windows requested a restart.'
    }

    $postInstallTarget = Get-ExactInstalledTarget -Descriptor $Package.Descriptor
    if (-not [string]::Equals([string]$postInstallTarget.ExecutablePath, [string]$Target.ExecutablePath, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The local NSIS installer returned success, but the configured installed executable path changed from '$($Target.ExecutablePath)' to '$($postInstallTarget.ExecutablePath)'."
    }
    $installedProof = Get-TrustedInstalledNsisPayloadProof -Path $postInstallTarget.ProofPath -NotBefore $installerStartedAt
    if (
        -not [string]::Equals($installedProof.PayloadSha256, [string]$Package.PayloadSha256, [StringComparison]::OrdinalIgnoreCase) -or
        $installedProof.PayloadLength -ne [int64]$Package.PayloadLength -or
        -not [string]::Equals($installedProof.Nonce, [string]$Package.PayloadNonce, [StringComparison]::Ordinal)
    ) {
        throw 'The local NSIS installer returned success, but its new installed-payload proof does not match this packaging run.'
    }
    $installedExecutable = Get-TrustedRegularFileFingerprint -Path $postInstallTarget.ExecutablePath -Description 'Executable installed by the local NSIS package'
    if (
        -not [string]::Equals($installedExecutable.Sha256, [string]$Package.PayloadSha256, [StringComparison]::OrdinalIgnoreCase) -or
        $installedExecutable.Length -ne [int64]$Package.PayloadLength
    ) {
        throw "The local NSIS installer returned success, but the installed executable does not match this packaging run. Expected SHA-256 '$($Package.PayloadSha256)', found '$($installedExecutable.Sha256)' at '$($Target.ExecutablePath)'."
    }
    $confirmedTarget = Get-ExactInstalledTarget -Descriptor $Package.Descriptor
    if (-not [string]::Equals([string]$confirmedTarget.ExecutablePath, [string]$postInstallTarget.ExecutablePath, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The installed executable path changed during verification from '$($postInstallTarget.ExecutablePath)' to '$($confirmedTarget.ExecutablePath)'."
    }
    $confirmedInstalledExecutable = Get-TrustedRegularFileFingerprint -Path $confirmedTarget.ExecutablePath -Description 'Installed executable stability confirmation'
    if (-not [string]::Equals($confirmedInstalledExecutable.Sha256, $installedExecutable.Sha256, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The installed executable changed during post-install verification at '$($confirmedTarget.ExecutablePath)'."
    }
    $confirmedInstalledProof = Get-TrustedInstalledNsisPayloadProof -Path $confirmedTarget.ProofPath -NotBefore $installerStartedAt
    if (
        -not [string]::Equals($confirmedInstalledProof.PayloadSha256, $installedProof.PayloadSha256, [StringComparison]::OrdinalIgnoreCase) -or
        $confirmedInstalledProof.PayloadLength -ne $installedProof.PayloadLength -or
        -not [string]::Equals($confirmedInstalledProof.Nonce, $installedProof.Nonce, [StringComparison]::Ordinal)
    ) {
        throw "The installed payload proof changed during post-install verification at '$($confirmedTarget.ProofPath)'."
    }

    Write-Host "Installed current worktree package at $($Target.ExecutablePath)"
    Write-Host "Installed executable SHA-256 verified: $($confirmedInstalledExecutable.Sha256)"
    Write-Host 'No iHub process was stopped or launched. Use the normal iHub Start Menu entry when you are ready to open the installed binary.'
    return [pscustomobject]@{
        ExecutablePath       = $confirmedInstalledExecutable.Path
        InstalledFingerprint = $confirmedInstalledExecutable.Sha256
        InstalledAt          = [DateTime]::UtcNow.ToString('o')
    }
}

function Get-DevelopmentPackageInstallScope {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    $descriptor = Get-CurrentNsisPackageDescriptor -RepositoryRoot $RepositoryRoot
    $target = Get-ExactInstalledTarget -Descriptor $descriptor
    return [pscustomobject]@{
        Descriptor = $descriptor
        Target     = $target
    }
}

function Assert-DevelopmentPackageInstallScopeUnchanged {
    param(
        [Parameter(Mandatory)]$ExpectedTarget,
        [Parameter(Mandatory)]$ActualScope
    )

    if (-not [string]::Equals(
            [IO.Path]::GetFullPath([string]$ExpectedTarget.ExecutablePath),
            [IO.Path]::GetFullPath([string]$ActualScope.Target.ExecutablePath),
            [StringComparison]::OrdinalIgnoreCase
        )) {
        throw "The iHub install target changed while waiting for the development package/install mutex. No NSIS artifact was cleared, built, or installed; rerun after src-tauri/tauri.conf.json is stable."
    }
}

function Invoke-PackageFromCurrentWorktree {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    $initialScope = Get-DevelopmentPackageInstallScope -RepositoryRoot $RepositoryRoot
    return Invoke-WithDevelopmentPackageInstallMutex -Target $initialScope.Target -Action {
        # Re-read after ownership is acquired. If a source save changed the
        # installed target while this process was waiting, fail before clearing
        # a bundle under a mutex derived for a different target.
        $lockedScope = Get-DevelopmentPackageInstallScope -RepositoryRoot $RepositoryRoot
        Assert-DevelopmentPackageInstallScopeUnchanged -ExpectedTarget $initialScope.Target -ActualScope $lockedScope
        Build-CurrentSignedNsisPackage -RepositoryRoot $RepositoryRoot
    }
}

function Invoke-InstallLatestFromCurrentWorktree {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [AllowEmptyString()]
        [string]$WatchStopSignalPath
    )

    Stop-WatchInstallIfRequested -StopSignalPath $WatchStopSignalPath
    $initialScope = Get-DevelopmentPackageInstallScope -RepositoryRoot $RepositoryRoot
    Invoke-WithDevelopmentPackageInstallMutex -Target $initialScope.Target -WatchStopSignalPath $WatchStopSignalPath -Action {
        Stop-WatchInstallIfRequested -StopSignalPath $WatchStopSignalPath
        $lockedScope = Get-DevelopmentPackageInstallScope -RepositoryRoot $RepositoryRoot
        Assert-DevelopmentPackageInstallScopeUnchanged -ExpectedTarget $initialScope.Target -ActualScope $lockedScope

        # This check runs only after the mutex is held. A different watcher or
        # manual -InstallLatest invocation cannot pass it, clear artifacts,
        # and race the same target while this invocation is packaging.
        Assert-ExactInstalledExecutableIsNotRunning -ExecutablePath $lockedScope.Target.ExecutablePath
        Stop-WatchInstallIfRequested -StopSignalPath $WatchStopSignalPath
        $builtPackage = Build-CurrentSignedNsisPackage -RepositoryRoot $RepositoryRoot
        Stop-WatchInstallIfRequested -StopSignalPath $WatchStopSignalPath
        $target = Get-ExactInstalledTarget -Descriptor $builtPackage.Descriptor
        Assert-DevelopmentPackageInstallScopeUnchanged -ExpectedTarget $initialScope.Target -ActualScope ([pscustomobject]@{ Target = $target })
        return Install-CurrentNsisPackage -Package $builtPackage -Target $target
    }
}

function Invoke-DevelopmentInstallWatch {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][ValidateRange(1, 300)][int]$IntervalSeconds,
        [AllowEmptyString()]
        [string]$WatchStopSignalPath,
        [AllowEmptyString()]
        [string]$WatchStatusPath
    )

    $persistedStatus = Get-DevelopmentInstallWatchStatusState -StatusPath $WatchStatusPath
    $installedBinaryFingerprint = $persistedStatus.InstalledFingerprint
    $lastSuccessAt = $persistedStatus.LastSuccessAt
    $lastError = $persistedStatus.LastError

    if (Test-WatchInstallStopSignal -StopSignalPath $WatchStopSignalPath) {
        Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'stopped' -Message 'A user requested cooperative WatchInstall shutdown before the watcher started.' -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
        Write-Host 'WatchInstall stop signal is already present. No package was built, installed, launched, or stopped.'
        return
    }

    Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'watching' -Message 'Watching the current source; no install has succeeded during this watcher session yet.' -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
    Write-Host "WatchInstall is monitoring saved iHub source files every $IntervalSeconds second(s). Press Ctrl+C to stop."
    Write-Host 'It does not fetch, pull, reset, checkout, clean, launch iHub, or stop any process.'
    if (-not [string]::IsNullOrWhiteSpace($WatchStopSignalPath)) {
        Write-Host "It will also exit cooperatively when this literal stop-signal file exists: $([IO.Path]::GetFullPath($WatchStopSignalPath))"
    }

    $lastObservedFingerprint = $null
    $stableObservations = 0
    $lastInstalledFingerprint = $null
    $lastFailedFingerprint = $null
    $failedAttemptCount = 0
    $failedRetryAt = [DateTime]::MinValue
    $automaticRetryDelaysSeconds = @(30, 120, 300)
    $lastBlockedPidSet = $null
    $lastBlockedMutexName = $null

    while ($true) {
        if (Test-WatchInstallStopSignal -StopSignalPath $WatchStopSignalPath) {
            Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'stopped' -Message 'The watcher observed the cooperative shutdown signal; no iHub process was stopped.' -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
            Write-Host 'WatchInstall stop signal detected. Exiting cooperatively; no iHub process was stopped.'
            return
        }

        $fingerprint = Get-DevelopmentSourceFingerprint -RepositoryRoot $RepositoryRoot
        if ($fingerprint -eq $lastObservedFingerprint) {
            $stableObservations++
        }
        else {
            $lastObservedFingerprint = $fingerprint
            $stableObservations = 0
            $lastFailedFingerprint = $null
            $failedAttemptCount = 0
            $failedRetryAt = [DateTime]::MinValue
            # A new source snapshot deserves one fresh, explicit wait message
            # even when the same installed iHub PID is still running. This is
            # observability only: WatchInstall continues to never stop it.
            $lastBlockedPidSet = $null
            $lastBlockedMutexName = $null
            Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'pending' -Message "Detected saved source snapshot $($fingerprint.Substring(0, 12)); waiting for a stable poll before packaging." -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
            Write-Host "Detected saved source change ($($fingerprint.Substring(0, 12))); waiting for one stable poll before packaging."
        }

        $failedRetryIsDue = (
            $fingerprint -eq $lastFailedFingerprint -and
            $failedAttemptCount -le $automaticRetryDelaysSeconds.Count -and
            [DateTime]::UtcNow -ge $failedRetryAt
        )
        if (
            $fingerprint -ne $lastInstalledFingerprint -and
            $stableObservations -ge 1 -and
            ($fingerprint -ne $lastFailedFingerprint -or $failedRetryIsDue)
        ) {
            $descriptor = Get-CurrentNsisPackageDescriptor -RepositoryRoot $RepositoryRoot
            $target = Get-ExactInstalledTarget -Descriptor $descriptor
            $processState = Get-ExactInstalledIHubProcessState -ExecutablePath $target.ExecutablePath
            if ($processState.ExactMatches.Count -gt 0) {
                $pidSet = $processState.ExactMatches -join ','
                if ($pidSet -ne $lastBlockedPidSet) {
                    Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'waiting' -Message "Waiting for the exact installed iHub process to close (PID $pidSet); no process will be stopped." -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                    Write-Warning "WatchInstall is waiting for the exact installed iHub process to close (PID $pidSet): $($processState.ExpectedPath). No process will be stopped."
                }
                $lastBlockedPidSet = $pidSet
                $lastBlockedMutexName = $null
            }
            else {
                $lastBlockedPidSet = $null
                try {
                    if ($null -eq $lastBlockedMutexName) {
                        Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'installing' -Message "Packaging and installing saved source snapshot $($fingerprint.Substring(0, 12))." -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                        Write-Host "Packaging and installing source snapshot $($fingerprint.Substring(0, 12))..."
                    }
                    $installResult = Invoke-InstallLatestFromCurrentWorktree -RepositoryRoot $RepositoryRoot -WatchStopSignalPath $WatchStopSignalPath
                    if ($null -eq $installResult -or [string]$installResult.InstalledFingerprint -notmatch '^[0-9a-f]{64}$') {
                        throw 'WatchInstall did not receive a verified installed executable fingerprint from InstallLatest.'
                    }
                    # Keep the pre-build fingerprint. If a save happens during
                    # packaging, the next poll sees a different fingerprint and
                    # schedules one more exact-current install.
                    $lastInstalledFingerprint = $fingerprint
                    $installedBinaryFingerprint = ([string]$installResult.InstalledFingerprint).ToLowerInvariant()
                    $lastSuccessAt = [string]$installResult.InstalledAt
                    $lastError = $null
                    $lastFailedFingerprint = $null
                    $failedAttemptCount = 0
                    $failedRetryAt = [DateTime]::MinValue
                    $lastBlockedMutexName = $null
                    Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'healthy' -Message "Installed and verified saved source snapshot $($fingerprint.Substring(0, 12))." -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $null
                    Write-Host "WatchInstall completed for source snapshot $($fingerprint.Substring(0, 12))."
                }
                catch {
                    if (Test-IHubExceptionMarker -ErrorRecord $_ -Marker 'iHubWatchInstallStopSignal') {
                        Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'stopped' -Message 'The watcher observed the cooperative shutdown signal; no iHub process was stopped.' -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                        Write-Host 'WatchInstall stop signal detected. Exiting cooperatively; no iHub process was stopped.'
                        return
                    }
                    if (Test-IHubExceptionMarker -ErrorRecord $_ -Marker 'iHubDevelopmentPackageInstallMutexTimeout') {
                        $mutexName = [string]$_.Exception.Data['iHubDevelopmentPackageInstallMutexName']
                        if ($mutexName -ne $lastBlockedMutexName) {
                            Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'waiting' -Message 'Waiting for another local development package/install operation; this watcher will retry without altering NSIS artifacts.' -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                            Write-Warning "WatchInstall is waiting for another local development package/install operation targeting $($target.ExecutablePath). No NSIS artifact was altered by this watcher; it will retry after the next poll."
                        }
                        # A lock timeout is transient, unlike a source or build
                        # error. Keep this fingerprint eligible for a retry
                        # after the other owner releases its mutex.
                        $lastBlockedMutexName = $mutexName
                    }
                    else {
                        $lastBlockedMutexName = $null
                        $afterFailure = Get-ExactInstalledIHubProcessState -ExecutablePath $target.ExecutablePath
                        if ($afterFailure.ExactMatches.Count -gt 0) {
                            $lastBlockedPidSet = $afterFailure.ExactMatches -join ','
                            Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'waiting' -Message 'iHub opened while packaging; installation was not forced and will be retried after it closes.' -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                            Write-Warning "iHub opened while WatchInstall was packaging; installation was not forced. Close it to allow a retry."
                        }
                        else {
                            $lastFailedFingerprint = $fingerprint
                            $failedAttemptCount++
                            $lastError = $_.Exception.Message
                            if ($failedAttemptCount -le $automaticRetryDelaysSeconds.Count) {
                                $delaySeconds = $automaticRetryDelaysSeconds[$failedAttemptCount - 1]
                                $failedRetryAt = [DateTime]::UtcNow.AddSeconds($delaySeconds)
                                $retryAtText = $failedRetryAt.ToString('o')
                                Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'retrying' -Message "Install attempt $failedAttemptCount failed for saved source snapshot $($fingerprint.Substring(0, 12)); retrying at $retryAtText." -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                                Write-Warning "WatchInstall attempt $failedAttemptCount failed for source snapshot $($fingerprint.Substring(0, 12)): $($_.Exception.Message) It will retry automatically in $delaySeconds second(s)."
                            }
                            else {
                                $failedRetryAt = [DateTime]::MaxValue
                                Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $RepositoryRoot -State 'failed' -Message "Install failed $failedAttemptCount times for saved source snapshot $($fingerprint.Substring(0, 12)); the bounded automatic retry limit was reached and another source save is required." -InstalledFingerprint $installedBinaryFingerprint -LastSuccessAt $lastSuccessAt -LastError $lastError
                                Write-Warning "WatchInstall failed $failedAttemptCount times for source snapshot $($fingerprint.Substring(0, 12)): $($_.Exception.Message) The bounded automatic retry limit was reached; save another source change after fixing the problem."
                            }
                        }
                    }
                }
            }
        }

        Start-Sleep -Seconds $IntervalSeconds
    }
}

if ([Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'scripts/dev.ps1 is for Windows. Use scripts/dev.sh on macOS.'
}

if ($Help) {
    Show-Usage
    return
}

$modeCount = 0
foreach ($mode in @([bool]$Build, [bool]$Package, [bool]$InstallLatest, [bool]$WatchInstall, [bool]$VerifyOnly)) {
    if ($mode) {
        $modeCount++
    }
}
if ($modeCount -gt 1) {
    throw 'Use only one of -Build, -Package, -InstallLatest, -WatchInstall, or -VerifyOnly.'
}
if ($Update -and $UpdateIfClean) {
    throw 'Use either -Update for strict behavior or -UpdateIfClean for best-effort safe behavior, not both.'
}
if (($InstallLatest -or $WatchInstall) -and ($Update -or $UpdateIfClean)) {
    throw '-InstallLatest and -WatchInstall always package the currently checked-out worktree and never update Git. Run an update mode separately, review the result, then start the local installation mode.'
}
if (-not $WatchInstall -and -not [string]::IsNullOrWhiteSpace($WatchStatusPath)) {
    throw '-WatchStatusPath is an internal persistent-service option and can only be used with -WatchInstall.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
foreach ($requiredFile in @('package.json', 'pnpm-lock.yaml', 'src-tauri/tauri.conf.json')) {
    if (-not (Test-Path -LiteralPath (Join-Path $repositoryRoot $requiredFile) -PathType Leaf)) {
        throw "This does not look like an iHub checkout: missing $requiredFile."
    }
}

Push-Location -LiteralPath $repositoryRoot
try {
    if ($WatchInstall -and (Test-WatchInstallStopSignal -StopSignalPath $WatchStopSignalPath)) {
        $persistedStatus = Get-DevelopmentInstallWatchStatusState -StatusPath $WatchStatusPath
        Write-DevelopmentInstallWatchStatus -StatusPath $WatchStatusPath -RepositoryRoot $repositoryRoot -State 'stopped' -Message 'A user requested cooperative WatchInstall shutdown before prerequisite work started.' -InstalledFingerprint $persistedStatus.InstalledFingerprint -LastSuccessAt $persistedStatus.LastSuccessAt -LastError $persistedStatus.LastError
        Write-Host 'WatchInstall stop signal is already present. No dependency sync, package, installation, launch, or process stop was attempted.'
        return
    }

    foreach ($command in @('git', 'node', 'corepack', 'cargo')) {
        Require-Command -Name $command
    }

    $insideWorkTree = Get-GitOutput -GitArguments @('rev-parse', '--is-inside-work-tree')
    if ($insideWorkTree -ne 'true') {
        throw 'scripts/dev.ps1 must run from an iHub Git worktree.'
    }

    $nodeVersion = (& node --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $nodeVersion -notmatch '^v(?<major>[0-9]+)\.(?<minor>[0-9]+)\.(?<patch>[0-9]+)') {
        throw "Could not parse Node.js version '$nodeVersion'."
    }
    $nodeMajor = [int]$Matches['major']
    $nodeMinor = [int]$Matches['minor']
    if ($nodeMajor -lt 22 -or ($nodeMajor -eq 22 -and $nodeMinor -lt 12)) {
        throw "iHub requires Node.js 22.12 or newer; found $nodeVersion."
    }

    Invoke-External -Executable 'corepack' -CommandArguments @('pnpm', '--version')

    $workTreeChanges = Get-GitOutput -GitArguments @('status', '--porcelain=v1', '--untracked-files=normal')
    if ($Update -or $UpdateIfClean) {
        Invoke-SafeFastForward -WorkTreeChanges $workTreeChanges -ContinueOnSkip:$UpdateIfClean
    }
    elseif (-not [string]::IsNullOrWhiteSpace($workTreeChanges)) {
        Write-Warning 'Starting from the current dirty worktree. No fetch, pull, reset, checkout, or clean operation is performed.'
    }
    else {
        Write-Host 'Starting from the currently checked-out source. Use -Update for a strict fast-forward or -UpdateIfClean to follow upstream when safe.'
    }

    $officialPluginSyncMode = if ($Update) {
        '--update'
    }
    elseif ($UpdateIfClean) {
        '--update-if-clean'
    }
    else {
        '--locked'
    }
    Write-Host "Preparing independent official plugin checkouts ($officialPluginSyncMode)..."
    Invoke-External -Executable 'node' -CommandArguments @(
        'scripts/bootstrap-official-plugins.mjs',
        $officialPluginSyncMode
    )

    # Fail before dependency work when the exact per-user target is still in
    # use. The same check runs again immediately before NSIS starts, so a
    # process opened during the build cannot be replaced either.
    $installLatestTarget = $null
    if ($InstallLatest) {
        $installLatestTarget = Get-ExactInstalledTarget -Descriptor (Get-CurrentNsisPackageDescriptor -RepositoryRoot $repositoryRoot)
        Assert-ExactInstalledExecutableIsNotRunning -ExecutablePath $installLatestTarget.ExecutablePath
    }

    if (-not $SkipInstall) {
        Write-Host 'Synchronizing dependencies from pnpm-lock.yaml (frozen; package versions are not upgraded)...'
        Invoke-Pnpm -PnpmArguments @('install', '--frozen-lockfile')
    }
    else {
        Write-Warning 'Skipping dependency synchronization by request.'
    }

    if (-not $SkipCheck) {
        Write-Host 'Checking TypeScript before launch...'
        Invoke-Pnpm -PnpmArguments @('check')
    }
    else {
        Write-Warning 'Skipping TypeScript check by request.'
    }

    if ($VerifyOnly) {
        Write-Host 'Development environment verification completed. No app was launched.'
        return
    }

    if ($Build) {
        Write-Host 'Building the current source without an installer bundle...'
        Invoke-Pnpm -PnpmArguments @('tauri', 'build', '--no-bundle')
        return
    }

    if ($Package) {
        Write-Host 'Building native installer artifacts from the current source...'
        # PowerShell variable names are case-insensitive. Do not assign to
        # $package here: it is the script's [switch]$Package parameter.
        $builtPackage = Invoke-PackageFromCurrentWorktree -RepositoryRoot $repositoryRoot
        Write-Host "Fresh signed NSIS installer: $($builtPackage.State.InstallerPath)"
        Write-Host "NSIS updater signature sidecar: $($builtPackage.State.SignaturePath)"
        Write-Host "Installer SHA-256: $($builtPackage.Sha256)"
        Write-Host "Installer artifacts are under $($builtPackage.Descriptor.BundleRoot)"
        return
    }

    if ($InstallLatest) {
        Write-Host 'Building a fresh signed NSIS package from the current worktree before installation...'
        Invoke-InstallLatestFromCurrentWorktree -RepositoryRoot $repositoryRoot
        return
    }

    if ($WatchInstall) {
        Invoke-DevelopmentInstallWatch -RepositoryRoot $repositoryRoot -IntervalSeconds $WatchIntervalSeconds -WatchStopSignalPath $WatchStopSignalPath -WatchStatusPath $WatchStatusPath
        return
    }

    Write-Host 'Launching iHub from the current source. Tauri/Vite will rebuild and reload as files change.'
    Invoke-Pnpm -PnpmArguments @('tauri', 'dev')
}
finally {
    Pop-Location
}
