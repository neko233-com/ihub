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
Assert-NoForcedProcessTermination -Ast $installAst -Label 'install-dev.ps1'
Assert-NoForcedProcessTermination -Ast $developmentAst -Label 'dev.ps1'

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

    # Build only the two trusted-launcher files that the read-only task-object
    # preflight requires. Calling -NoLaunch here would create real Start Menu
    # shortcuts because Windows resolves that known folder independently of an
    # overridden APPDATA environment variable.
    $smokeInstallRoot = Join-Path $smokeLocalAppData 'iHub Development'
    New-Item -ItemType Directory -Path $smokeInstallRoot -Force | Out-Null
    $smokeMarker = [ordered]@{
        schemaVersion    = 1
        managedBy        = 'iHub Development Launcher'
        launcherRevision = 3
        sourceRoot       = $repositoryRoot
        installedAt      = [DateTime]::UtcNow.ToString('o')
    }
    Set-Content -LiteralPath (Join-Path $smokeInstallRoot 'launcher.json') -Value ($smokeMarker | ConvertTo-Json) -Encoding UTF8 -NoNewline
    Set-Content -LiteralPath (Join-Path $smokeInstallRoot 'Launch iHub Development.ps1') -Value "[CmdletBinding()]`r`nparam()" -Encoding UTF8 -NoNewline

    & $installScriptPath -EnablePersistentDevelopmentInstall -UpstreamCheckMinutes 30 -WhatIf
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
