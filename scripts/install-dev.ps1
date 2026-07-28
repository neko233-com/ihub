# Installs a user-local iHub Development launcher. It never copies, downloads,
# or mutates the source checkout: current-source launches and the explicit
# package-and-install action both delegate to scripts/dev.ps1 in that worktree.

[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [switch]$NoLaunch,

    [switch]$Update,

    [switch]$UpdateIfClean,

    [switch]$InstallLatest,

    [switch]$WatchInstall,

    [ValidateRange(1, 300)]
    [int]$WatchIntervalSeconds = 2,

    # The persistent service is deliberately opt-in.  Normal launcher setup
    # does not create a scheduled task or background process.
    [switch]$EnablePersistentDevelopmentInstall,

    [switch]$DisablePersistentDevelopmentInstall,

    [switch]$DevelopmentInstallStatus,

    [ValidateRange(10, 240)]
    [int]$UpstreamCheckMinutes = 30,

    [switch]$Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-AtomicUtf8File {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content
    )

    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (".$([IO.Path]::GetFileName($Path)).$([guid]::NewGuid().ToString('N')).tmp")
    try {
        Set-Content -LiteralPath $temporaryPath -Value $Content -Encoding UTF8 -NoNewline
        Move-Item -LiteralPath $temporaryPath -Destination $Path -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporaryPath) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
}

function Show-Usage {
    @'
Usage: .\scripts\install-dev.ps1 [options]

  -NoLaunch  Install or refresh the Start Menu launchers without opening iHub.
  -Update    Safely fetch and fast-forward the configured worktree before the
             first launch. With -NoLaunch, it updates and verifies without
             opening a window. Dirty or diverged worktrees are never changed.
  -UpdateIfClean  Try the same safe update before launch, but continue with
                  current saved source when the worktree cannot be updated.
  -InstallLatest  Build, validate, and silently install the configured current
                  worktree's local signed NSIS package. It never updates Git
                  or stops a running iHub process.
  -WatchInstall   Explicitly watch saved local source files and keep the
                   configured current worktree installed after stable changes.
                   It never updates Git, launches iHub, or stops processes.
  -WatchIntervalSeconds  Poll interval for -WatchInstall (1-300; default: 2).
  -EnablePersistentDevelopmentInstall  Opt in to two current-user, limited
                   Windows scheduled tasks: a local source watcher and a safe
                   upstream refresh loop. Run -NoLaunch once first so the
                   trusted launcher exists. It never uses SYSTEM, elevation,
                   a password, -Command, or ihub.exe as a task action.
  -DisablePersistentDevelopmentInstall  Request cooperative service shutdown
                   and unregister only iHub-owned persistent tasks. It never
                   stops iHub or a scheduled task process.
  -DevelopmentInstallStatus  Show the local launcher and persistent task
                   state without changing files or Task Scheduler.
  -UpstreamCheckMinutes  Safe upstream refresh cadence when enabling the
                   persistent service (10-240; default: 30).
  -Help      Show this help.

Five Start Menu entries are installed:
  iHub Development (Always Latest, Safe) Follow upstream when it is safe.
  iHub Development (Current Source)      Launch the current saved worktree.
  iHub Development (Update & Launch)     Require a safe fast-forward first.
  iHub Development (Install Current Build)  Refresh the local installed binary.
  iHub Development (Watch & Install Current Build)  Follow saved local files.

The persistent development service is disabled by default. It can replace a
closed installed iHub only after a verified current-source package is ready;
it never replaces a running iHub process.
'@ | Write-Host
}

function Assert-OwnedInstallRoot {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$MarkerPath
    )

    if (-not (Test-Path -LiteralPath $Root)) {
        New-Item -ItemType Directory -Path $Root | Out-Null
        return
    }

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        throw "Developer launcher path exists but is not a directory: $Root"
    }
    if (-not (Test-Path -LiteralPath $MarkerPath -PathType Leaf)) {
        throw "Refusing to reuse $Root because it is not an iHub Development launcher directory. No files were changed."
    }

    try {
        $marker = Get-Content -LiteralPath $MarkerPath -Raw | ConvertFrom-Json
    }
    catch {
        throw "Refusing to reuse $Root because its launcher marker is invalid. No files were changed."
    }
    if ($marker.managedBy -ne 'iHub Development Launcher') {
        throw "Refusing to reuse $Root because it belongs to another application. No files were changed."
    }
}

if ([Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'scripts/install-dev.ps1 is for Windows only.'
}
if ($Help) {
    Show-Usage
    return
}
if ($Update -and $UpdateIfClean) {
    throw 'Use either -Update for strict behavior or -UpdateIfClean for best-effort safe behavior, not both.'
}
if (($InstallLatest -or $WatchInstall) -and ($Update -or $UpdateIfClean)) {
    throw '-InstallLatest and -WatchInstall package the current worktree without changing Git. Run an update mode separately, review the result, then start the local installation mode.'
}
if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    throw 'LOCALAPPDATA is unavailable; cannot create a user-local developer launcher.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$developerScript = Join-Path $repositoryRoot 'scripts\dev.ps1'
$iconPath = Join-Path $repositoryRoot 'src-tauri\icons\icon.ico'
foreach ($requiredPath in @($developerScript, (Join-Path $repositoryRoot '.git'))) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "This does not look like an iHub Git checkout: missing $requiredPath."
    }
}

$installRoot = Join-Path $env:LOCALAPPDATA 'iHub Development'
$markerPath = Join-Path $installRoot 'launcher.json'
$launcherPath = Join-Path $installRoot 'Launch iHub Development.ps1'
$persistentStopSignalPath = Join-Path $installRoot 'persistent-development-install.stop'
$persistentWatcherWrapperPath = Join-Path $installRoot 'Run iHub Development Watch Service.ps1'
$persistentRefreshWrapperPath = Join-Path $installRoot 'Run iHub Development Safe Refresh.ps1'
$persistentWatcherStatusPath = Join-Path $installRoot 'persistent-development-watch-status.json'
$persistentRefreshStatusPath = Join-Path $installRoot 'persistent-development-refresh-status.json'
$persistentTaskDescription = 'iHub Development persistent install service v1; managedBy=iHub Development Launcher'
$persistentWatcherTaskName = 'iHub Development - Watch & Install'
$persistentRefreshTaskName = 'iHub Development - Safe Upstream Refresh'

function Assert-ScheduledTasksSupport {
    foreach ($commandName in @(
            'Get-ScheduledTask',
            'Get-ScheduledTaskInfo',
            'New-ScheduledTaskAction',
            'New-ScheduledTaskTrigger',
            'New-ScheduledTaskPrincipal',
            'New-ScheduledTaskSettingsSet',
            'New-ScheduledTask',
            'Register-ScheduledTask',
            'Unregister-ScheduledTask',
            'Start-ScheduledTask',
            'Get-CimInstance'
        )) {
        if ($null -eq (Get-Command $commandName -ErrorAction SilentlyContinue)) {
            throw "Windows Scheduled Tasks support is unavailable: missing $commandName. No persistent development service was changed."
        }
    }
}

function Get-TrustedExistingDevelopmentLauncher {
    param(
        [Parameter(Mandatory)][string]$ExpectedSourceRoot
    )

    if (-not (Test-Path -LiteralPath $installRoot -PathType Container)) {
        throw "No iHub Development launcher is installed at $installRoot. First run .\\scripts\\install-dev.ps1 -NoLaunch, then explicitly enable the persistent service."
    }

    foreach ($expectedFile in @($markerPath, $launcherPath)) {
        if (-not (Test-Path -LiteralPath $expectedFile -PathType Leaf)) {
            throw "The trusted iHub Development launcher file is missing: $expectedFile. Re-run .\\scripts\\install-dev.ps1 -NoLaunch before enabling the persistent service."
        }
        $item = Get-Item -LiteralPath $expectedFile -Force
        if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "Refusing to use an unsafe iHub Development launcher file: $($item.FullName)"
        }
    }

    try {
        $launcherMarker = Get-Content -LiteralPath $markerPath -Raw | ConvertFrom-Json
    }
    catch {
        throw "The iHub Development launcher marker is invalid: $markerPath. No persistent task was changed."
    }

    $managedByProperty = $launcherMarker.PSObject.Properties['managedBy']
    $sourceRootProperty = $launcherMarker.PSObject.Properties['sourceRoot']
    $launcherRevisionProperty = $launcherMarker.PSObject.Properties['launcherRevision']
    $launcherRevision = 0
    if ($null -ne $launcherRevisionProperty) {
        [void][int]::TryParse([string]$launcherRevisionProperty.Value, [ref]$launcherRevision)
    }
    if ($null -eq $managedByProperty -or $null -eq $sourceRootProperty -or $managedByProperty.Value -ne 'iHub Development Launcher' -or [string]::IsNullOrWhiteSpace([string]$sourceRootProperty.Value)) {
        throw "The iHub Development launcher marker is not trusted: $markerPath. No persistent task was changed."
    }
    if ($launcherRevision -lt 3) {
        throw "The iHub Development launcher is older than the verified-install status protocol. Re-run .\\scripts\\install-dev.ps1 -NoLaunch before enabling the service."
    }

    try {
        $configuredSourceRoot = [IO.Path]::GetFullPath([string]$sourceRootProperty.Value)
        $normalizedExpectedSourceRoot = [IO.Path]::GetFullPath($ExpectedSourceRoot)
    }
    catch {
        throw "The configured iHub Development source root is invalid. Re-run .\\scripts\\install-dev.ps1 -NoLaunch from the intended worktree."
    }
    if (-not [string]::Equals($configuredSourceRoot, $normalizedExpectedSourceRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The existing iHub Development launcher points to '$configuredSourceRoot', not this worktree '$normalizedExpectedSourceRoot'. Re-run .\\scripts\\install-dev.ps1 -NoLaunch from this worktree before enabling its persistent service."
    }

    return [pscustomobject]@{
        SourceRoot = $configuredSourceRoot
        Marker     = $launcherMarker
    }
}

function Get-IHubPersistentTaskRecord {
    param([Parameter(Mandatory)][string]$TaskName)

    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -eq $task) {
        return [pscustomobject]@{
            Name        = $TaskName
            Exists      = $false
            Owned       = $false
            Description = $null
            State       = $null
            LastResult  = $null
            LastRunTime = $null
            Task        = $null
        }
    }

    $taskInfo = $null
    try {
        $taskInfo = Get-ScheduledTaskInfo -InputObject $task -ErrorAction Stop
    }
    catch {
        # A task may be deleted between the two read-only queries. Status is
        # still useful, and enable/disable will re-read and verify ownership.
    }

    return [pscustomobject]@{
        Name        = $TaskName
        Exists      = $true
        Owned       = ([string]$task.Description -eq $persistentTaskDescription)
        Description = [string]$task.Description
        State       = [string]$task.State
        LastResult  = if ($null -eq $taskInfo) { $null } else { $taskInfo.LastTaskResult }
        LastRunTime = if ($null -eq $taskInfo) { $null } else { $taskInfo.LastRunTime }
        Task        = $task
    }
}

function Assert-IHubPersistentTaskOwnership {
    param([Parameter(Mandatory)]$TaskRecord)

    if ($TaskRecord.Exists -and -not $TaskRecord.Owned) {
        throw "Refusing to change scheduled task '$($TaskRecord.Name)' because it does not carry the iHub Development ownership marker. No task was overwritten or removed."
    }
}

function ConvertTo-PowerShellFileArguments {
    param([Parameter(Mandatory)][string]$ScriptPath)

    if ($ScriptPath.IndexOf([char]34) -ge 0) {
        throw "The persistent service script path contains an unsupported quote character: $ScriptPath"
    }
    return "-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$ScriptPath`""
}

function Get-IHubPersistentPowerShellExecutable {
    $powershellCommand = Get-Command powershell.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1
    if ($null -eq $powershellCommand -or [string]::IsNullOrWhiteSpace([string]$powershellCommand.Source)) {
        throw 'Windows PowerShell is unavailable; persistent development tasks were not changed.'
    }
    return [IO.Path]::GetFullPath([string]$powershellCommand.Source)
}

function Get-IHubPersistentWrapperProcesses {
    param([Parameter(Mandatory)][string]$PowerShellExecutable)

    $expectedExecutable = [IO.Path]::GetFullPath($PowerShellExecutable)
    $wrapperPatterns = @(
        $persistentWatcherWrapperPath
        $persistentRefreshWrapperPath
    ) | ForEach-Object {
        '(?i)(?:^|\s)-File\s+"' + [regex]::Escape([IO.Path]::GetFullPath($_)) + '"\s*$'
    }

    $matchingProcesses = @()
    foreach ($process in @(Get-CimInstance Win32_Process -Filter "Name = 'powershell.exe'" -ErrorAction Stop)) {
        if (
            [string]::IsNullOrWhiteSpace([string]$process.ExecutablePath) -or
            [string]::IsNullOrWhiteSpace([string]$process.CommandLine)
        ) {
            continue
        }
        try {
            $processExecutable = [IO.Path]::GetFullPath([string]$process.ExecutablePath)
        }
        catch {
            continue
        }
        if (-not [string]::Equals($processExecutable, $expectedExecutable, [StringComparison]::OrdinalIgnoreCase)) {
            continue
        }
        foreach ($pattern in $wrapperPatterns) {
            if ([string]$process.CommandLine -match $pattern) {
                $matchingProcesses += $process
                break
            }
        }
    }
    return @($matchingProcesses)
}

function Wait-IHubPersistentWrapperProcessesToExit {
    param(
        [Parameter(Mandatory)][string]$PowerShellExecutable,
        [ValidateRange(1, 1800)][int]$TimeoutSeconds = 300
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $announced = $false
    while ($true) {
        $processes = @(Get-IHubPersistentWrapperProcesses -PowerShellExecutable $PowerShellExecutable)
        if ($processes.Count -eq 0) {
            if ($announced) {
                Write-Host 'All previous iHub persistent service instances exited cooperatively.'
            }
            return
        }
        if (-not $announced) {
            $processIds = ($processes | ForEach-Object { [string]$_.ProcessId }) -join ', '
            Write-Host "Waiting for previous iHub persistent service instance(s) to exit cooperatively (PID $processIds). No process will be stopped."
            $announced = $true
        }
        if ([DateTime]::UtcNow -ge $deadline) {
            $processIds = ($processes | ForEach-Object { [string]$_.ProcessId }) -join ', '
            throw "Timed out waiting for previous iHub persistent service instance(s) to exit cooperatively (PID $processIds). The stop request remains in place, and no replacement task was registered."
        }
        Start-Sleep -Seconds 1
    }
}

function Get-IHubPersistentScopedMutexName {
    param([Parameter(Mandatory)][ValidateSet('Management', 'Watch', 'Refresh')][string]$Purpose)

    $scope = "$(([IO.Path]::GetFullPath($installRoot)).ToUpperInvariant())|$Purpose"
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $scopeBytes = [Text.Encoding]::UTF8.GetBytes($scope)
        $scopeHash = ([BitConverter]::ToString($sha256.ComputeHash($scopeBytes))).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }
    return "Global\iHub-PersistentDevelopment-$Purpose-$scopeHash"
}

function Invoke-WithIHubPersistentManagementMutex {
    param([Parameter(Mandatory)][scriptblock]$Action)

    $mutex = [Threading.Mutex]::new($false, (Get-IHubPersistentScopedMutexName -Purpose Management))
    $ownsMutex = $false
    try {
        try {
            $ownsMutex = $mutex.WaitOne([TimeSpan]::FromMinutes(6))
        }
        catch [Threading.AbandonedMutexException] {
            $ownsMutex = $true
        }
        if (-not $ownsMutex) {
            throw 'Another iHub persistent development service management operation is still active. No task was changed.'
        }
        return & $Action
    }
    finally {
        if ($ownsMutex) {
            $mutex.ReleaseMutex()
        }
        $mutex.Dispose()
    }
}

function Write-IHubPersistentStopSignal {
    if (Test-Path -LiteralPath $persistentStopSignalPath) {
        $stopSignal = Get-Item -LiteralPath $persistentStopSignalPath -Force
        if ($stopSignal.PSIsContainer -or (($stopSignal.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
            throw "Refusing to replace an unsafe persistent-service stop signal: $($stopSignal.FullName)"
        }
    }
    Write-AtomicUtf8File -Path $persistentStopSignalPath -Content ([DateTime]::UtcNow.ToString('o'))
}

function Remove-IHubPersistentStopSignal {
    if (-not (Test-Path -LiteralPath $persistentStopSignalPath)) {
        return
    }

    $stopSignal = Get-Item -LiteralPath $persistentStopSignalPath -Force
    if ($stopSignal.PSIsContainer -or (($stopSignal.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "Refusing to remove an unsafe persistent-service stop signal: $($stopSignal.FullName)"
    }
    Remove-Item -LiteralPath $stopSignal.FullName -Force
}

function Write-IHubPersistentServiceWrappers {
    param([Parameter(Mandatory)][ValidateRange(10, 240)][int]$RefreshMinutes)

    $watcherMutexName = Get-IHubPersistentScopedMutexName -Purpose Watch
    $refreshMutexName = Get-IHubPersistentScopedMutexName -Purpose Refresh

    # The wrappers keep Task Scheduler actions deliberately boring: each action
    # is an exact local PowerShell -File path. The scripts themselves validate
    # the existing launcher marker before delegating to dev.ps1.
    $watcherWrapperContent = @'
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$launcherPath = Join-Path $PSScriptRoot 'Launch iHub Development.ps1'
$stopSignalPath = Join-Path $PSScriptRoot 'persistent-development-install.stop'
$statusPath = Join-Path $PSScriptRoot 'persistent-development-watch-status.json'

function Write-ServiceStatus {
    param(
        [Parameter(Mandatory)][string]$State,
        [Parameter(Mandatory)][string]$Message
    )

    $sourceRoot = $null
    $installedFingerprint = $null
    $lastSuccessAt = $null
    $lastError = $null
    try {
        $marker = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'launcher.json') -Raw | ConvertFrom-Json
        $sourceRoot = [string]$marker.sourceRoot
    }
    catch {
        $sourceRoot = $null
    }

    if (Test-Path -LiteralPath $statusPath) {
        $existingItem = Get-Item -LiteralPath $statusPath -Force
        if ($existingItem.PSIsContainer -or (($existingItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) -or $existingItem.Length -gt 65536) {
            throw "Refusing to replace an unsafe WatchInstall status file: $statusPath"
        }
        try {
            $existingStatus = Get-Content -LiteralPath $existingItem.FullName -Raw | ConvertFrom-Json
            if ($existingStatus.managedBy -eq 'iHub Development persistent install service v1' -and $existingStatus.service -eq 'watch-install') {
                if ($null -ne $existingStatus.PSObject.Properties['installedFingerprint'] -and [string]$existingStatus.installedFingerprint -match '^[0-9a-fA-F]{64}$') {
                    $installedFingerprint = ([string]$existingStatus.installedFingerprint).ToLowerInvariant()
                }
                if ($null -ne $existingStatus.PSObject.Properties['lastSuccessAt'] -and -not [string]::IsNullOrWhiteSpace([string]$existingStatus.lastSuccessAt)) {
                    $lastSuccessAt = [string]$existingStatus.lastSuccessAt
                }
                if ($null -ne $existingStatus.PSObject.Properties['lastError'] -and -not [string]::IsNullOrWhiteSpace([string]$existingStatus.lastError)) {
                    $lastError = [string]$existingStatus.lastError
                }
            }
        }
        catch {
            # A safe regular but malformed status file has no trustworthy
            # previous success fields. The new non-healthy state replaces it.
            $installedFingerprint = $null
            $lastSuccessAt = $null
            $lastError = $null
        }
    }
    if ($State -eq 'failed') {
        $lastError = $Message
    }

    $payload = [ordered]@{
        schemaVersion        = 2
        managedBy            = 'iHub Development persistent install service v1'
        service              = 'watch-install'
        sourceRoot           = $sourceRoot
        state                = $State
        message              = $Message
        updatedAt            = [DateTime]::UtcNow.ToString('o')
        installedFingerprint = $installedFingerprint
        lastSuccessAt        = $lastSuccessAt
        lastError            = $lastError
    }
    $temporaryPath = Join-Path $PSScriptRoot ('.persistent-development-watch-status.' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        Set-Content -LiteralPath $temporaryPath -Value ($payload | ConvertTo-Json -Depth 5) -Encoding UTF8 -NoNewline
        Move-Item -LiteralPath $temporaryPath -Destination $statusPath -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporaryPath) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
}

function Wait-ServiceMutexOrStop {
    param([Parameter(Mandatory)][string]$MutexName)

    $mutex = [Threading.Mutex]::new($false, $MutexName)
    $ownsMutex = $false
    while (-not $ownsMutex) {
        if (Test-Path -LiteralPath $stopSignalPath) {
            $mutex.Dispose()
            return $null
        }
        try {
            $ownsMutex = $mutex.WaitOne(1000)
        }
        catch [Threading.AbandonedMutexException] {
            $ownsMutex = $true
        }
    }
    if (Test-Path -LiteralPath $stopSignalPath) {
        $mutex.ReleaseMutex()
        $mutex.Dispose()
        return $null
    }
    return $mutex
}

$serviceMutex = Wait-ServiceMutexOrStop -MutexName '__IHUB_WATCH_MUTEX_NAME__'
if ($null -eq $serviceMutex) {
    Write-ServiceStatus -State 'stopped' -Message 'A user requested persistent development service shutdown before this watcher instance acquired ownership.'
    exit 0
}

$serviceExitCode = 0
try {
    if (Test-Path -LiteralPath $stopSignalPath) {
        Write-ServiceStatus -State 'stopped' -Message 'A user requested persistent development service shutdown before the watcher started.'
    }
    else {
        try {
            # dev.ps1 owns the long-running state transitions so every verified
            # install or failed attempt is visible while this wrapper owns the
            # cross-task-definition service mutex.
            Write-ServiceStatus -State 'starting' -Message 'Validating the configured source before the WatchInstall loop starts.'
            & $launcherPath -WatchInstall -WatchIntervalSeconds 5 -WatchStopSignalPath $stopSignalPath -WatchStatusPath $statusPath
            if ($LASTEXITCODE -ne 0) {
                throw "The iHub Development watcher exited with code $LASTEXITCODE."
            }
            if (Test-Path -LiteralPath $stopSignalPath) {
                Write-ServiceStatus -State 'stopped' -Message 'The watcher observed the user shutdown signal and exited without stopping iHub.'
            }
            else {
                Write-ServiceStatus -State 'failed' -Message 'The watcher exited unexpectedly; Task Scheduler may retry it.'
                $serviceExitCode = 1
            }
        }
        catch {
            Write-ServiceStatus -State 'failed' -Message $_.Exception.Message
            $serviceExitCode = 1
        }
    }
}
finally {
    $serviceMutex.ReleaseMutex()
    $serviceMutex.Dispose()
}
if ($serviceExitCode -ne 0) {
    exit $serviceExitCode
}
'@

    $refreshWrapperContent = @'
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$launcherPath = Join-Path $PSScriptRoot 'Launch iHub Development.ps1'
$stopSignalPath = Join-Path $PSScriptRoot 'persistent-development-install.stop'
$statusPath = Join-Path $PSScriptRoot 'persistent-development-refresh-status.json'
$refreshMinutes = __IHUB_REFRESH_MINUTES__

function Write-ServiceStatus {
    param(
        [Parameter(Mandatory)][string]$State,
        [Parameter(Mandatory)][string]$Message
    )

    $sourceRoot = $null
    try {
        $marker = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'launcher.json') -Raw | ConvertFrom-Json
        $sourceRoot = [string]$marker.sourceRoot
    }
    catch {
        $sourceRoot = $null
    }
    $payload = [ordered]@{
        schemaVersion = 1
        managedBy     = 'iHub Development persistent install service v1'
        service       = 'safe-upstream-refresh'
        sourceRoot    = $sourceRoot
        state         = $State
        message       = $Message
        updatedAt     = [DateTime]::UtcNow.ToString('o')
    }
    $temporaryPath = Join-Path $PSScriptRoot ('.persistent-development-refresh-status.' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        Set-Content -LiteralPath $temporaryPath -Value ($payload | ConvertTo-Json -Depth 5) -Encoding UTF8 -NoNewline
        Move-Item -LiteralPath $temporaryPath -Destination $statusPath -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporaryPath) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
}

function Wait-ServiceMutexOrStop {
    param([Parameter(Mandatory)][string]$MutexName)

    $mutex = [Threading.Mutex]::new($false, $MutexName)
    $ownsMutex = $false
    while (-not $ownsMutex) {
        if (Test-Path -LiteralPath $stopSignalPath) {
            $mutex.Dispose()
            return $null
        }
        try {
            $ownsMutex = $mutex.WaitOne(1000)
        }
        catch [Threading.AbandonedMutexException] {
            $ownsMutex = $true
        }
    }
    if (Test-Path -LiteralPath $stopSignalPath) {
        $mutex.ReleaseMutex()
        $mutex.Dispose()
        return $null
    }
    return $mutex
}

$serviceMutex = Wait-ServiceMutexOrStop -MutexName '__IHUB_REFRESH_MUTEX_NAME__'
if ($null -eq $serviceMutex) {
    Write-ServiceStatus -State 'stopped' -Message 'A user requested persistent development service shutdown before this refresh instance acquired ownership.'
    exit 0
}

try {
    while (-not (Test-Path -LiteralPath $stopSignalPath)) {
        try {
            Write-ServiceStatus -State 'checking' -Message 'Attempting a safe upstream refresh only when the worktree is clean and can fast-forward.'
            & $launcherPath -UpdateIfClean -VerifyOnly -SkipInstall -SkipCheck
            if ($LASTEXITCODE -ne 0) {
                throw "The iHub Development safe refresh exited with code $LASTEXITCODE."
            }
            Write-ServiceStatus -State 'waiting' -Message "Last safe upstream refresh attempt completed; next attempt is in $refreshMinutes minute(s)."
        }
        catch {
            Write-ServiceStatus -State 'retrying' -Message $_.Exception.Message
        }

        $remainingSeconds = $refreshMinutes * 60
        while ($remainingSeconds -gt 0 -and -not (Test-Path -LiteralPath $stopSignalPath)) {
            $sleepSeconds = [Math]::Min(5, $remainingSeconds)
            Start-Sleep -Seconds $sleepSeconds
            $remainingSeconds -= $sleepSeconds
        }
    }

    Write-ServiceStatus -State 'stopped' -Message 'A user requested persistent development service shutdown. No process was stopped by the script.'
}
finally {
    $serviceMutex.ReleaseMutex()
    $serviceMutex.Dispose()
}
'@

    $watcherWrapperContent = $watcherWrapperContent.Replace('__IHUB_WATCH_MUTEX_NAME__', $watcherMutexName)
    $refreshWrapperContent = $refreshWrapperContent.Replace('__IHUB_REFRESH_MUTEX_NAME__', $refreshMutexName)
    $refreshWrapperContent = $refreshWrapperContent.Replace('__IHUB_REFRESH_MINUTES__', [string]$RefreshMinutes)
    Write-AtomicUtf8File -Path $persistentWatcherWrapperPath -Content $watcherWrapperContent
    Write-AtomicUtf8File -Path $persistentRefreshWrapperPath -Content $refreshWrapperContent
}

function New-IHubPersistentScheduledTask {
    param(
        [Parameter(Mandatory)][string]$TaskName,
        [Parameter(Mandatory)][string]$WrapperPath,
        [Parameter(Mandatory)][string]$CurrentUserName
    )

    $powershellExecutable = Get-IHubPersistentPowerShellExecutable
    $action = New-ScheduledTaskAction -Execute $powershellExecutable -Argument (ConvertTo-PowerShellFileArguments -ScriptPath $WrapperPath) -WorkingDirectory $installRoot
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $CurrentUserName
    $principal = New-ScheduledTaskPrincipal -UserId $CurrentUserName -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    return New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description $persistentTaskDescription
}

function Enable-IHubPersistentDevelopmentInstall {
    [CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
    param([Parameter(Mandatory)][ValidateRange(10, 240)][int]$RefreshMinutes)

    $null = Get-TrustedExistingDevelopmentLauncher -ExpectedSourceRoot $repositoryRoot
    Assert-ScheduledTasksSupport
    $preflightTasks = @(
        Get-IHubPersistentTaskRecord -TaskName $persistentWatcherTaskName
        Get-IHubPersistentTaskRecord -TaskName $persistentRefreshTaskName
    )
    foreach ($preflightTask in $preflightTasks) {
        Assert-IHubPersistentTaskOwnership -TaskRecord $preflightTask
    }
    $currentUserName = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    if ([string]::IsNullOrWhiteSpace($currentUserName)) {
        throw 'Could not determine the current Windows user for the limited persistent development tasks.'
    }
    $powershellExecutable = Get-IHubPersistentPowerShellExecutable

    # Construct every task before writing a wrapper or registering anything, so
    # unsupported task settings fail closed without changing local state.
    $watcherTask = New-IHubPersistentScheduledTask -TaskName $persistentWatcherTaskName -WrapperPath $persistentWatcherWrapperPath -CurrentUserName $currentUserName
    $refreshTask = New-IHubPersistentScheduledTask -TaskName $persistentRefreshTaskName -WrapperPath $persistentRefreshWrapperPath -CurrentUserName $currentUserName

    $transactionTarget = "Task Scheduler\\$persistentWatcherTaskName and Task Scheduler\\$persistentRefreshTaskName"
    if (-not $PSCmdlet.ShouldProcess($transactionTarget, 'cooperatively hand off and register the complete iHub persistent development service')) {
        Write-Host 'Would enable the default-off iHub Development persistent install service for the current Windows user.'
        return
    }

    Invoke-WithIHubPersistentManagementMutex -Action {
        $existingTasks = @(
            Get-IHubPersistentTaskRecord -TaskName $persistentWatcherTaskName
            Get-IHubPersistentTaskRecord -TaskName $persistentRefreshTaskName
        )
        foreach ($existingTask in $existingTasks) {
            Assert-IHubPersistentTaskOwnership -TaskRecord $existingTask
        }

        $handoffStarted = $false
        try {
            # A task definition can be replaced while its former engine process
            # is still alive. Signal first, remove every restart source, and
            # then wait for all exact wrapper command lines—including orphaned
            # former task instances—to leave cooperatively before publishing
            # new wrappers.
            Write-IHubPersistentStopSignal
            $handoffStarted = $true
            foreach ($existingTask in $existingTasks) {
                if ($existingTask.Exists) {
                    Unregister-ScheduledTask -TaskName $existingTask.Name -Confirm:$false
                }
            }
            Wait-IHubPersistentWrapperProcessesToExit -PowerShellExecutable $powershellExecutable
            Write-IHubPersistentServiceWrappers -RefreshMinutes $RefreshMinutes

            # Keep the stop signal present until both definitions exist. An
            # unexpected immediate trigger can therefore only exit safely.
            Register-ScheduledTask -TaskName $persistentWatcherTaskName -InputObject $watcherTask | Out-Null
            Register-ScheduledTask -TaskName $persistentRefreshTaskName -InputObject $refreshTask | Out-Null
            Remove-IHubPersistentStopSignal
            Start-ScheduledTask -TaskName $persistentWatcherTaskName
            Start-ScheduledTask -TaskName $persistentRefreshTaskName
        }
        catch {
            $setupError = $_
            if ($handoffStarted) {
                try {
                    Write-IHubPersistentStopSignal
                }
                catch {
                    Write-Warning "Could not restore the cooperative stop signal after persistent-service setup failed: $($_.Exception.Message)"
                }
                foreach ($taskName in @($persistentWatcherTaskName, $persistentRefreshTaskName)) {
                    try {
                        $rollbackTask = Get-IHubPersistentTaskRecord -TaskName $taskName
                        if ($rollbackTask.Exists) {
                            if (-not $rollbackTask.Owned) {
                                Write-Warning "A non-iHub task appeared during rollback and was not changed: $taskName"
                                continue
                            }
                            Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
                        }
                    }
                    catch {
                        Write-Warning "Could not remove owned task '$taskName' during rollback: $($_.Exception.Message)"
                    }
                }
                try {
                    Wait-IHubPersistentWrapperProcessesToExit -PowerShellExecutable $powershellExecutable
                }
                catch {
                    Write-Warning $_.Exception.Message
                }
            }
            throw $setupError
        }
    }

    Write-Host 'Enabled the default-off iHub Development persistent install service for the current Windows user.'
    Write-Host "  Watch task:   $persistentWatcherTaskName"
    Write-Host "  Refresh task: $persistentRefreshTaskName (every $RefreshMinutes minute(s) while signed in)"
    Write-Host 'It only installs a verified local package after you close the exact installed iHub yourself; it never stops iHub or uses administrator privileges.'
}

function Disable-IHubPersistentDevelopmentInstall {
    [CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
    param()

    Assert-ScheduledTasksSupport
    $null = Get-TrustedExistingDevelopmentLauncher -ExpectedSourceRoot $repositoryRoot
    $powershellExecutable = Get-IHubPersistentPowerShellExecutable
    $transactionTarget = "Task Scheduler\\$persistentWatcherTaskName and Task Scheduler\\$persistentRefreshTaskName"
    if (-not $PSCmdlet.ShouldProcess($transactionTarget, 'request cooperative shutdown and unregister the complete iHub persistent development service')) {
        Write-Host 'Would disable future iHub Development persistent service starts. No iHub or scheduled-task process would be stopped.'
        return
    }

    Invoke-WithIHubPersistentManagementMutex -Action {
        $existingTasks = @(
            Get-IHubPersistentTaskRecord -TaskName $persistentWatcherTaskName
            Get-IHubPersistentTaskRecord -TaskName $persistentRefreshTaskName
        )
        foreach ($existingTask in $existingTasks) {
            Assert-IHubPersistentTaskOwnership -TaskRecord $existingTask
        }

        Write-IHubPersistentStopSignal
        foreach ($existingTask in $existingTasks) {
            if ($existingTask.Exists) {
                Unregister-ScheduledTask -TaskName $existingTask.Name -Confirm:$false
            }
        }
        Wait-IHubPersistentWrapperProcessesToExit -PowerShellExecutable $powershellExecutable
    }

    Write-Host 'Disabled future iHub Development persistent service starts. Every prior wrapper instance exited cooperatively; no iHub or scheduled-task process was stopped.'
}

function Get-IHubPersistentServiceStatusFile {
    param([Parameter(Mandatory)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    try {
        $item = Get-Item -LiteralPath $Path -Force
        if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) -or $item.Length -gt 65536) {
            return [ordered]@{ state = 'invalid'; message = 'The status file is not a safe small regular file.' }
        }
        $serviceStatus = Get-Content -LiteralPath $item.FullName -Raw | ConvertFrom-Json
        if ($serviceStatus.managedBy -ne 'iHub Development persistent install service v1') {
            return [ordered]@{ state = 'invalid'; message = 'The status file has no iHub Development ownership marker.' }
        }
        $installedFingerprint = if ($null -eq $serviceStatus.PSObject.Properties['installedFingerprint'] -or [string]::IsNullOrWhiteSpace([string]$serviceStatus.installedFingerprint)) {
            $null
        }
        else {
            ([string]$serviceStatus.installedFingerprint).ToLowerInvariant()
        }
        $lastSuccessAt = if ($null -eq $serviceStatus.PSObject.Properties['lastSuccessAt'] -or [string]::IsNullOrWhiteSpace([string]$serviceStatus.lastSuccessAt)) {
            $null
        }
        else {
            [string]$serviceStatus.lastSuccessAt
        }
        $lastError = if ($null -eq $serviceStatus.PSObject.Properties['lastError'] -or [string]::IsNullOrWhiteSpace([string]$serviceStatus.lastError)) {
            $null
        }
        else {
            [string]$serviceStatus.lastError
        }
        $reportedState = [string]$serviceStatus.state
        $healthy = (
            $reportedState -eq 'healthy' -and
            $installedFingerprint -match '^[0-9a-f]{64}$' -and
            $null -ne $lastSuccessAt -and
            $null -eq $lastError
        )
        if ($reportedState -eq 'healthy' -and -not $healthy) {
            return [ordered]@{
                state                = 'invalid'
                message              = 'The watcher claimed healthy without a verified fingerprint, success time, or cleared error.'
                updatedAt            = [string]$serviceStatus.updatedAt
                healthy              = $false
                installedFingerprint = $installedFingerprint
                lastSuccessAt        = $lastSuccessAt
                lastError            = $lastError
            }
        }
        return [ordered]@{
            state                = $reportedState
            message              = [string]$serviceStatus.message
            updatedAt            = [string]$serviceStatus.updatedAt
            healthy              = $healthy
            installedFingerprint = $installedFingerprint
            lastSuccessAt        = $lastSuccessAt
            lastError            = $lastError
        }
    }
    catch {
        return [ordered]@{ state = 'invalid'; message = $_.Exception.Message }
    }
}

function Show-IHubPersistentDevelopmentInstallStatus {
    Assert-ScheduledTasksSupport
    $markerState = 'missing'
    $configuredSourceRoot = $null
    $launcherRevision = 0
    if (Test-Path -LiteralPath $markerPath -PathType Leaf) {
        try {
            $existingMarker = Get-Content -LiteralPath $markerPath -Raw | ConvertFrom-Json
            if ($existingMarker.managedBy -eq 'iHub Development Launcher') {
                if ($null -ne $existingMarker.PSObject.Properties['launcherRevision']) {
                    [void][int]::TryParse([string]$existingMarker.launcherRevision, [ref]$launcherRevision)
                }
                $markerState = if ($launcherRevision -ge 3) { 'trusted' } else { 'refresh-required' }
                $configuredSourceRoot = [string]$existingMarker.sourceRoot
            }
            else {
                $markerState = 'foreign-or-invalid'
            }
        }
        catch {
            $markerState = 'foreign-or-invalid'
        }
    }

    $taskRecords = @(
        Get-IHubPersistentTaskRecord -TaskName $persistentWatcherTaskName
        Get-IHubPersistentTaskRecord -TaskName $persistentRefreshTaskName
    )
    $status = [ordered]@{
        schemaVersion        = 2
        managedBy            = 'iHub Development persistent install service v1'
        launcherRoot         = $installRoot
        launcherMarker       = $markerState
        launcherRevision     = $launcherRevision
        configuredSourceRoot = $configuredSourceRoot
        stopRequested        = (Test-Path -LiteralPath $persistentStopSignalPath)
        watcherService       = Get-IHubPersistentServiceStatusFile -Path $persistentWatcherStatusPath
        refreshService       = Get-IHubPersistentServiceStatusFile -Path $persistentRefreshStatusPath
        tasks                = @($taskRecords | ForEach-Object {
                [ordered]@{
                    name        = $_.Name
                    exists      = $_.Exists
                    owned       = $_.Owned
                    state       = $_.State
                    lastResult  = $_.LastResult
                    lastRunTime = $_.LastRunTime
                }
            })
    }
    $status | ConvertTo-Json -Depth 6
}

$persistentModeCount = 0
foreach ($persistentMode in @([bool]$EnablePersistentDevelopmentInstall, [bool]$DisablePersistentDevelopmentInstall, [bool]$DevelopmentInstallStatus)) {
    if ($persistentMode) {
        $persistentModeCount++
    }
}
if ($persistentModeCount -gt 1) {
    throw 'Use only one of -EnablePersistentDevelopmentInstall, -DisablePersistentDevelopmentInstall, or -DevelopmentInstallStatus.'
}
if ($persistentModeCount -eq 1) {
    if ($NoLaunch -or $Update -or $UpdateIfClean -or $InstallLatest -or $WatchInstall) {
        throw 'Persistent development service management cannot be combined with launch, update, install, or one-shot watch options.'
    }
    if ($EnablePersistentDevelopmentInstall) {
        Enable-IHubPersistentDevelopmentInstall -RefreshMinutes $UpstreamCheckMinutes
    }
    elseif ($DisablePersistentDevelopmentInstall) {
        Disable-IHubPersistentDevelopmentInstall
    }
    else {
        Show-IHubPersistentDevelopmentInstallStatus
    }
    return
}

Assert-OwnedInstallRoot -Root $installRoot -MarkerPath $markerPath

$marker = [ordered]@{
    schemaVersion = 1
    managedBy     = 'iHub Development Launcher'
    launcherRevision = 3
    sourceRoot    = $repositoryRoot
    installedAt   = [DateTime]::UtcNow.ToString('o')
}
Write-AtomicUtf8File -Path $markerPath -Content ($marker | ConvertTo-Json)

$launcherContent = @'
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

    [string]$WatchStopSignalPath,

    [string]$WatchStatusPath,

    [switch]$VerifyOnly,

    [switch]$Help
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$markerPath = Join-Path $PSScriptRoot 'launcher.json'
if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
    throw "iHub Development launcher configuration is missing: $markerPath"
}

try {
    $marker = Get-Content -LiteralPath $markerPath -Raw | ConvertFrom-Json
}
catch {
    throw "iHub Development launcher configuration is invalid: $markerPath"
}

if ($marker.managedBy -ne 'iHub Development Launcher' -or [string]::IsNullOrWhiteSpace([string]$marker.sourceRoot)) {
    throw 'iHub Development launcher configuration is not trusted.'
}

$sourceRoot = [IO.Path]::GetFullPath([string]$marker.sourceRoot)
$developerScript = Join-Path $sourceRoot 'scripts\dev.ps1'
if (-not (Test-Path -LiteralPath $developerScript -PathType Leaf)) {
    throw "The configured iHub source checkout is unavailable: $sourceRoot. Re-run its scripts/install-dev.ps1 after moving or restoring the checkout."
}

$developerArguments = @{}
if ($Update) { $developerArguments.Update = $true }
if ($UpdateIfClean) { $developerArguments.UpdateIfClean = $true }
if ($SkipInstall) { $developerArguments.SkipInstall = $true }
if ($SkipCheck) { $developerArguments.SkipCheck = $true }
if ($Build) { $developerArguments.Build = $true }
if ($Package) { $developerArguments.Package = $true }
if ($InstallLatest) { $developerArguments.InstallLatest = $true }
if ($WatchInstall) {
    $developerArguments.WatchInstall = $true
    $developerArguments.WatchIntervalSeconds = $WatchIntervalSeconds
    if (-not [string]::IsNullOrWhiteSpace($WatchStopSignalPath)) {
        $developerArguments.WatchStopSignalPath = $WatchStopSignalPath
    }
    if (-not [string]::IsNullOrWhiteSpace($WatchStatusPath)) {
        $developerArguments.WatchStatusPath = $WatchStatusPath
    }
}
if ($VerifyOnly) { $developerArguments.VerifyOnly = $true }
if ($Help) { $developerArguments.Help = $true }

& $developerScript @developerArguments
exit 0
'@
Write-AtomicUtf8File -Path $launcherPath -Content $launcherContent

$startMenuRoot = [Environment]::GetFolderPath([Environment+SpecialFolder]::StartMenu)
if ([string]::IsNullOrWhiteSpace($startMenuRoot)) {
    throw 'The Windows Start Menu folder is unavailable; launcher files were created but no shortcut was written.'
}
$shortcutDirectory = Join-Path $startMenuRoot 'Programs\iHub'
New-Item -ItemType Directory -Path $shortcutDirectory -Force | Out-Null

function Write-DeveloperShortcut {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Arguments,
        [Parameter(Mandatory)][string]$Description
    )

    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($Path)
    $shortcut.TargetPath = (Get-Command powershell.exe -ErrorAction Stop).Source
    $shortcut.Arguments = $Arguments
    $shortcut.WorkingDirectory = $repositoryRoot
    $shortcut.Description = $Description
    if (Test-Path -LiteralPath $iconPath -PathType Leaf) {
        $shortcut.IconLocation = "$iconPath,0"
    }
    $shortcut.Save()
}

$currentSourceShortcutPath = Join-Path $shortcutDirectory 'iHub Development (Current Source).lnk'
$alwaysLatestShortcutPath = Join-Path $shortcutDirectory 'iHub Development (Always Latest, Safe).lnk'
$updateAndLaunchShortcutPath = Join-Path $shortcutDirectory 'iHub Development (Update & Launch).lnk'
$installCurrentBuildShortcutPath = Join-Path $shortcutDirectory 'iHub Development (Install Current Build).lnk'
$watchInstallCurrentBuildShortcutPath = Join-Path $shortcutDirectory 'iHub Development (Watch & Install Current Build).lnk'
$baseArguments = "-NoLogo -NoProfile -ExecutionPolicy Bypass -File `"$launcherPath`""
Write-DeveloperShortcut -Path $currentSourceShortcutPath -Arguments $baseArguments -Description 'Launch iHub from the configured current-source development worktree.'
Write-DeveloperShortcut -Path $alwaysLatestShortcutPath -Arguments "$baseArguments -UpdateIfClean" -Description 'Follow the configured iHub upstream when a clean fast-forward is safe, otherwise launch the current saved source.'
Write-DeveloperShortcut -Path $updateAndLaunchShortcutPath -Arguments "$baseArguments -Update" -Description 'Safely fast-forward a clean iHub worktree, then launch it.'
Write-DeveloperShortcut -Path $installCurrentBuildShortcutPath -Arguments "$baseArguments -InstallLatest" -Description 'Build, validate, and install the configured current iHub worktree without changing Git or stopping iHub.'
Write-DeveloperShortcut -Path $watchInstallCurrentBuildShortcutPath -Arguments "$baseArguments -WatchInstall" -Description 'Explicitly watch and install saved current iHub source changes without changing Git or stopping iHub.'

Write-Host 'iHub Development launcher installed.'
Write-Host "  Source worktree: $repositoryRoot"
Write-Host "  Launcher:        $launcherPath"
Write-Host "  Always latest:   $alwaysLatestShortcutPath"
Write-Host "  Start Menu:      $currentSourceShortcutPath"
Write-Host "  Update launcher: $updateAndLaunchShortcutPath"
Write-Host "  Install launcher: $installCurrentBuildShortcutPath"
Write-Host "  Watch installer:  $watchInstallCurrentBuildShortcutPath"
Write-Host 'The always-latest shortcut fast-forwards only a clean, non-diverged worktree and otherwise launches current saved source. The current-source shortcut never updates Git. The install and watch shortcuts package only this exact local worktree and never stop iHub.'

if ($NoLaunch) {
    if ($Update) {
        Write-Host 'Safely updating and verifying the configured source without launching iHub...'
        & $launcherPath -Update -VerifyOnly
        exit 0
    }

    if ($UpdateIfClean) {
        Write-Host 'Safely following upstream when possible, then verifying the configured source without launching iHub...'
        & $launcherPath -UpdateIfClean -VerifyOnly
        exit 0
    }

    if ($InstallLatest) {
        Write-Host 'Building, validating, and installing the current source without launching iHub...'
        & $launcherPath -InstallLatest
        exit 0
    }

    if ($WatchInstall) {
        Write-Host 'Watching and installing saved current-source changes without launching iHub...'
        & $launcherPath -WatchInstall -WatchIntervalSeconds $WatchIntervalSeconds
        exit 0
    }

    exit 0
}

if ($InstallLatest) {
    Write-Host 'Building, validating, and installing the current source...'
    & $launcherPath -InstallLatest
    exit 0
}

if ($WatchInstall) {
    Write-Host 'Watching and installing saved current-source changes...'
    & $launcherPath -WatchInstall -WatchIntervalSeconds $WatchIntervalSeconds
    exit 0
}

Write-Host 'Launching the development source now...'
if ($Update) {
    & $launcherPath -Update
}
elseif ($UpdateIfClean) {
    & $launcherPath -UpdateIfClean
}
else {
    & $launcherPath -UpdateIfClean
}
exit 0
