[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'scripts/verify-windows-development-scripts.ps1 is for Windows only.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$installScriptPath = Join-Path $repositoryRoot 'scripts\install-dev.ps1'
$developmentScriptPath = Join-Path $repositoryRoot 'scripts\dev.ps1'
$publicInstallScriptPath = Join-Path $repositoryRoot 'scripts\install.ps1'
$backgroundProcessModulePath = Join-Path $repositoryRoot 'src-tauri\src\background_process.rs'
$rustSourceRoot = Join-Path $repositoryRoot 'src-tauri\src'
$nsisHookPath = Join-Path $repositoryRoot 'src-tauri\windows\installer-hooks.nsh'

function Read-PowerShellAst {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Label
    )

    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $Path,
        [ref]$tokens,
        [ref]$parseErrors
    )
    if ($parseErrors.Count -gt 0) {
        $details = ($parseErrors | ForEach-Object {
                "line $($_.Extent.StartLineNumber): $($_.Message)"
            }) -join '; '
        throw "$Label does not parse: $details"
    }
    return $ast
}

function Assert-NoForcedProcessTermination {
    param(
        [Parameter(Mandatory)]$Ast,
        [Parameter(Mandatory)][string]$Label
    )

    $forbidden = @('Stop-Process', 'Stop-ScheduledTask')
    foreach ($command in @($Ast.FindAll({
                    param($node)
                    $node -is [System.Management.Automation.Language.CommandAst]
                }, $true))) {
        $commandName = $command.GetCommandName()
        if ($commandName -in $forbidden) {
            throw "$Label invokes forbidden command '$commandName' at line $($command.Extent.StartLineNumber)."
        }
        if (
            $commandName -eq 'Register-ScheduledTask' -and
            @($command.CommandElements | Where-Object {
                    $_ -is [System.Management.Automation.Language.CommandParameterAst] -and
                    $_.ParameterName -eq 'Force'
                }).Count -gt 0
        ) {
            throw "$Label uses Register-ScheduledTask -Force at line $($command.Extent.StartLineNumber)."
        }
    }
}

function Assert-OrderedSourceMarkers {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string[]]$Markers,
        [Parameter(Mandatory)][string]$Label
    )

    $cursor = -1
    foreach ($marker in $Markers) {
        $next = $Source.IndexOf($marker, $cursor + 1, [StringComparison]::Ordinal)
        if ($next -lt 0) {
            throw "$Label is missing required lifecycle marker '$marker'."
        }
        if ($next -le $cursor) {
            throw "$Label lifecycle marker '$marker' is out of order."
        }
        $cursor = $next
    }
}

$installAst = Read-PowerShellAst -Path $installScriptPath -Label 'install-dev.ps1'
$developmentAst = Read-PowerShellAst -Path $developmentScriptPath -Label 'dev.ps1'
$publicInstallAst = Read-PowerShellAst -Path $publicInstallScriptPath -Label 'install.ps1'
Assert-NoForcedProcessTermination -Ast $installAst -Label 'install-dev.ps1'
Assert-NoForcedProcessTermination -Ast $developmentAst -Label 'dev.ps1'
Assert-NoForcedProcessTermination -Ast $publicInstallAst -Label 'install.ps1'

# A release iHub process has no parent console. Every console-subsystem child
# therefore needs CREATE_NO_WINDOW at the Rust boundary, while every
# PowerShell entry point launched from Explorer or Task Scheduler must hide
# its own host before git/node/cargo/makensis inherit it.
$backgroundProcessSource = Get-Content -LiteralPath $backgroundProcessModulePath -Raw
foreach ($requiredMarker in @(
        'const CREATE_NO_WINDOW: u32 = 0x0800_0000;'
        'command.creation_flags(CREATE_NO_WINDOW);'
    )) {
    if (-not $backgroundProcessSource.Contains($requiredMarker)) {
        throw "background_process.rs is missing required Windows background-process marker '$requiredMarker'."
    }
}

$directRustConstructors = @()
foreach ($rustSource in @(Get-ChildItem -LiteralPath $rustSourceRoot -Recurse -File -Filter '*.rs')) {
    if ([string]::Equals($rustSource.FullName, $backgroundProcessModulePath, [StringComparison]::OrdinalIgnoreCase)) {
        continue
    }
    $source = Get-Content -LiteralPath $rustSource.FullName -Raw
    if ($source.Contains('Command::new(') -or $source.Contains('std::process::Command::new(')) {
        $directRustConstructors += $rustSource.Name
    }
}
if ($directRustConstructors.Count -gt 0) {
    throw "Rust child processes bypass background_command in: $($directRustConstructors -join ', ')."
}

$hiddenPowerShellArguments = '-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File'
$taskArgumentFunction = $installAst.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'ConvertTo-PowerShellFileArguments'
    }, $true)
if ($null -eq $taskArgumentFunction -or -not $taskArgumentFunction.Body.Extent.Text.Contains($hiddenPowerShellArguments)) {
    throw 'Persistent scheduled-task PowerShell arguments do not require a hidden, non-interactive host.'
}
$shortcutArgumentAssignment = $installAst.Find({
        param($node)
        $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and
        $node.Left -is [System.Management.Automation.Language.VariableExpressionAst] -and
        $node.Left.VariablePath.UserPath -eq 'baseArguments'
    }, $true)
if (
    $null -eq $shortcutArgumentAssignment -or
    -not $shortcutArgumentAssignment.Right.Extent.Text.Contains('//B //NoLogo') -or
    -not $shortcutArgumentAssignment.Right.Extent.Text.Contains('$launcherShimPath')
) {
    throw 'Development Start Menu shortcuts do not require the background WScript launcher shim.'
}
$shortcutTargetAssignment = $installAst.Find({
        param($node)
        $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and
        $node.Left.Extent.Text -eq '$shortcut.TargetPath'
    }, $true)
if (
    $null -eq $shortcutTargetAssignment -or
    -not $shortcutTargetAssignment.Right.Extent.Text.Contains('$systemWscriptPath')
) {
    throw 'Development Start Menu shortcuts still target a console-subsystem executable.'
}
$installScriptSource = Get-Content -LiteralPath $installScriptPath -Raw
foreach ($requiredShimMarker in @(
        "Join-Path `$env:SystemRoot 'System32\wscript.exe'"
        'Test-IHubSafeRegularFile -Path $systemWscriptPath'
        'CreateObject("WScript.Shell")'
        'shell.Run(commandLine, 0, False)'
        '-NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File'
    )) {
    if (-not $installScriptSource.Contains($requiredShimMarker)) {
        throw "The GUI development launcher shim is missing '$requiredShimMarker'."
    }
}

$nsisHookSource = Get-Content -LiteralPath $nsisHookPath -Raw
if (
    $nsisHookSource -match '(?i)!system\s+`[^`]*powershell(?:\.exe)?' -and
    $nsisHookSource -notmatch '(?i)!system\s+`[^`]*-NonInteractive\s+-WindowStyle\s+Hidden[^`]*`'
) {
    throw 'The NSIS build hook starts PowerShell without a hidden, non-interactive window.'
}

foreach ($scriptRecord in @(
        [pscustomobject]@{ Ast = $developmentAst; Label = 'dev.ps1' }
        [pscustomobject]@{ Ast = $publicInstallAst; Label = 'install.ps1' }
    )) {
    foreach ($command in @($scriptRecord.Ast.FindAll({
                    param($node)
                    $node -is [System.Management.Automation.Language.CommandAst] -and
                    $node.GetCommandName() -eq 'Start-Process'
                }, $true))) {
        if (
            $command.Extent.Text -match "(?i)(?:^|\s)(?:'|`")?/S(?:'|`")?(?:\s|$)" -and
            $command.Extent.Text -notmatch '(?i)(?:^|\s)-WindowStyle\s+Hidden(?:\s|$)'
        ) {
            throw "$($scriptRecord.Label) starts an unattended installer without -WindowStyle Hidden at line $($command.Extent.StartLineNumber)."
        }
    }
}

# Node's child_process defaults to creating a console window when its parent
# is a GUI-subsystem process. Every maintained script wrapper must opt into
# windowsHide so git, cmd.exe, Node, Cargo, and build helpers stay backgrounded
# even when a future caller launches the script outside an inherited console.
$nodeChildCallPattern = '\b(?:spawn|spawnSync|execFile|execFileSync|exec|execSync)\s*\('
foreach ($scriptFile in @(Get-ChildItem -LiteralPath (Join-Path $repositoryRoot 'scripts') -Recurse -File)) {
    if ($scriptFile.Extension -notin @('.js', '.mjs', '.cjs', '.sh')) {
        continue
    }
    $scriptSource = Get-Content -LiteralPath $scriptFile.FullName -Raw
    if ($scriptSource -notmatch 'node:child_process') {
        continue
    }
    $childCallCount = [regex]::Matches($scriptSource, $nodeChildCallPattern).Count
    $windowsHideCount = [regex]::Matches($scriptSource, '\bwindowsHide\s*:\s*true\b').Count
    if ($childCallCount -ne $windowsHideCount) {
        throw "$($scriptFile.Name) has $childCallCount Node child-process call(s) but $windowsHideCount windowsHide:true option(s)."
    }
}

$cargoManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'src-tauri\Cargo.toml') -Raw
if ($cargoManifest -match '(?i)tauri[-_]plugin[-_]shell') {
    throw 'The unrestricted Tauri shell process plugin must remain absent.'
}

$terminalLaunchPattern = '(?im)(?:open\s+(?:-a|--application)\s+["'']?Terminal(?:\.app)?|tell\s+application\s+["'']Terminal(?:\.app)?|Terminal\.app)'
foreach ($sourceRoot in @(
        (Join-Path $repositoryRoot 'src-tauri\src')
        (Join-Path $repositoryRoot 'scripts')
    )) {
    foreach ($sourceFile in @(Get-ChildItem -LiteralPath $sourceRoot -Recurse -File | Where-Object {
                $_.Extension -in @('.rs', '.ps1', '.sh', '.js', '.mjs', '.cjs')
            })) {
        if (
            [string]::Equals($sourceFile.FullName, $PSCommandPath, [StringComparison]::OrdinalIgnoreCase) -or
            [string]::Equals($sourceFile.FullName, $backgroundProcessModulePath, [StringComparison]::OrdinalIgnoreCase)
        ) {
            continue
        }
        $source = Get-Content -LiteralPath $sourceFile.FullName -Raw
        if ($source -match $terminalLaunchPattern) {
            throw "$($sourceFile.Name) contains an explicit macOS Terminal application launch."
        }
    }
}

$enableFunction = $installAst.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Enable-IHubPersistentDevelopmentInstall'
    }, $true)
$disableFunction = $installAst.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Disable-IHubPersistentDevelopmentInstall'
    }, $true)
if ($null -eq $enableFunction -or $null -eq $disableFunction) {
    throw 'install-dev.ps1 is missing persistent service lifecycle functions.'
}
Assert-OrderedSourceMarkers -Source $enableFunction.Body.Extent.Text -Label 'Enable lifecycle' -Markers @(
    'Write-IHubPersistentStopSignal'
    'Unregister-ScheduledTask'
    'Wait-IHubPersistentWrapperProcessesToExit'
    'Write-IHubPersistentServiceWrappers'
    'Register-ScheduledTask'
    'Remove-IHubPersistentStopSignal'
    'Start-ScheduledTask'
)
Assert-OrderedSourceMarkers -Source $disableFunction.Body.Extent.Text -Label 'Disable lifecycle' -Markers @(
    'Write-IHubPersistentStopSignal'
    'Unregister-ScheduledTask'
    'Wait-IHubPersistentWrapperProcessesToExit'
)

$wrapperAssignments = @($installAst.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and
            $node.Left -is [System.Management.Automation.Language.VariableExpressionAst] -and
            $node.Left.VariablePath.UserPath -in @('watcherWrapperContent', 'refreshWrapperContent') -and
            $node.Right -is [System.Management.Automation.Language.CommandExpressionAst] -and
            $node.Right.Expression -is [System.Management.Automation.Language.StringConstantExpressionAst]
        }, $true))
if ($wrapperAssignments.Count -ne 2) {
    throw "Expected two persistent-service wrapper templates, found $($wrapperAssignments.Count)."
}

foreach ($assignment in $wrapperAssignments) {
    $wrapperName = $assignment.Left.VariablePath.UserPath
    $wrapperContent = $assignment.Right.Expression.Value
    $wrapperContent = $wrapperContent.Replace('__IHUB_WATCH_MUTEX_NAME__', 'Global\iHub-Test-Watch-0123456789abcdef')
    $wrapperContent = $wrapperContent.Replace('__IHUB_REFRESH_MUTEX_NAME__', 'Global\iHub-Test-Refresh-0123456789abcdef')
    $wrapperContent = $wrapperContent.Replace('__IHUB_REFRESH_MINUTES__', '30')
    $wrapperTokens = $null
    $wrapperErrors = $null
    $wrapperAst = [System.Management.Automation.Language.Parser]::ParseInput(
        $wrapperContent,
        [ref]$wrapperTokens,
        [ref]$wrapperErrors
    )
    if ($wrapperErrors.Count -gt 0) {
        $details = ($wrapperErrors | ForEach-Object {
                "line $($_.Extent.StartLineNumber): $($_.Message)"
            }) -join '; '
        throw "$wrapperName does not parse after placeholder expansion: $details"
    }
    Assert-NoForcedProcessTermination -Ast $wrapperAst -Label $wrapperName
}

# Exercise task-object creation from pwsh without registering or starting a
# real task. This catches accidental reliance on `$PSHOME\powershell.exe`,
# which is absent when install-dev.ps1 is invoked from PowerShell 7.
$smokeRoot = Join-Path ([IO.Path]::GetTempPath()) ("ihub-development-script-smoke-" + [guid]::NewGuid().ToString('N'))
$smokeLocalAppData = Join-Path $smokeRoot 'LocalAppData'
$previousLocalAppData = $env:LOCALAPPDATA
try {
    Import-Module CimCmdlets -ErrorAction Stop
    Import-Module ScheduledTasks -ErrorAction Stop
    New-Item -ItemType Directory -Path $smokeLocalAppData -Force | Out-Null
    $env:LOCALAPPDATA = $smokeLocalAppData

    # Build only the trusted-launcher files that the read-only task-object
    # preflight requires. Calling -NoLaunch here would create real Start Menu
    # shortcuts because Windows resolves that known folder independently of an
    # overridden APPDATA environment variable.
    $smokeInstallRoot = Join-Path $smokeLocalAppData 'iHub Development'
    New-Item -ItemType Directory -Path $smokeInstallRoot -Force | Out-Null
    $smokeMarker = [ordered]@{
        schemaVersion    = 1
        managedBy        = 'iHub Development Launcher'
        launcherRevision = 4
        sourceRoot       = $repositoryRoot
        installedAt      = [DateTime]::UtcNow.ToString('o')
    }
    Set-Content -LiteralPath (Join-Path $smokeInstallRoot 'launcher.json') -Value ($smokeMarker | ConvertTo-Json) -Encoding UTF8 -NoNewline
    Set-Content -LiteralPath (Join-Path $smokeInstallRoot 'Launch iHub Development.ps1') -Value "[CmdletBinding()]`r`nparam()" -Encoding UTF8 -NoNewline
    Set-Content -LiteralPath (Join-Path $smokeInstallRoot 'Launch iHub Development.vbs') -Value "Option Explicit`r`nWScript.Quit 0" -Encoding UTF8 -NoNewline

    & $installScriptPath -EnablePersistentDevelopmentInstall -UpstreamCheckMinutes 30 -WhatIf

    $trustedStatus = ((& $installScriptPath -DevelopmentInstallStatus) -join [Environment]::NewLine) | ConvertFrom-Json
    if ($trustedStatus.launcherMarker -ne 'trusted') {
        throw "Development install status did not trust the complete revision-4 launcher fixture: $($trustedStatus.launcherMarker)"
    }
    $smokeShimPath = Join-Path $smokeInstallRoot 'Launch iHub Development.vbs'
    $smokeShimBackupPath = Join-Path $smokeInstallRoot 'Launch iHub Development.vbs.smoke-backup'
    [IO.File]::Move($smokeShimPath, $smokeShimBackupPath)
    try {
        $missingShimStatus = ((& $installScriptPath -DevelopmentInstallStatus) -join [Environment]::NewLine) | ConvertFrom-Json
        if ($missingShimStatus.launcherMarker -ne 'refresh-required') {
            throw "Development install status trusted a revision-4 marker with a missing WScript shim: $($missingShimStatus.launcherMarker)"
        }
    }
    finally {
        [IO.File]::Move($smokeShimBackupPath, $smokeShimPath)
    }
}
finally {
    $env:LOCALAPPDATA = $previousLocalAppData
    $resolvedSmokeRoot = [IO.Path]::GetFullPath($smokeRoot)
    $resolvedTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolvedSmokeRoot.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean an unexpected smoke-test path: $resolvedSmokeRoot"
    }
    if (Test-Path -LiteralPath $resolvedSmokeRoot) {
        Remove-Item -LiteralPath $resolvedSmokeRoot -Recurse -Force
    }
}

Write-Host 'Windows development PowerShell scripts passed syntax, safety, and pwsh task-creation smoke checks.'
