[CmdletBinding()]
param(
    [string]$WorkflowDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($WorkflowDirectory)) {
    $WorkflowDirectory = Join-Path $PSScriptRoot '..\.github\workflows'
}

$actionlint = Get-Command actionlint -ErrorAction SilentlyContinue
if ($null -eq $actionlint) {
    throw 'actionlint is required. Install it from https://github.com/rhysd/actionlint and rerun this script.'
}

$workflowFiles = @(
    Get-ChildItem -LiteralPath $WorkflowDirectory -File |
        Where-Object { $_.Extension -in @('.yml', '.yaml') } |
        Sort-Object -Property FullName
)
if ($workflowFiles.Count -eq 0) {
    throw "No workflow files found in $WorkflowDirectory"
}

Write-Host "Linting $($workflowFiles.Count) GitHub Actions workflow(s) with $($actionlint.Path)"
& $actionlint.Path $workflowFiles.FullName
if ($LASTEXITCODE -ne 0) {
    throw "actionlint failed with exit code $LASTEXITCODE"
}
