[CmdletBinding()]
param(
    [ValidateSet(100000, 500000, 1000000)]
    [int]$Entries = 100000,

    [ValidateRange(5, 101)]
    [int]$Samples = 21
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (($Samples % 2) -eq 0) {
    throw 'Samples must be odd so the P50/P95 nearest-rank values remain unambiguous.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $repositoryRoot 'src-tauri\Cargo.toml'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "Could not find the iHub Cargo manifest: $manifestPath"
}

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -eq $cargo) {
    throw 'cargo is required to run the local-search benchmark.'
}

# The ignored test creates a deterministic metadata-only fixture in memory. It
# never scans configured/default roots, starts iHub, creates a watcher, or
# writes an index snapshot. Cargo may update its normal build/dependency caches
# while compiling the test binary, but never an iHub index root or user content.
Write-Host "Running iHub synthetic local-search benchmark: $Entries input entries, $Samples samples/query."
Write-Host 'No user directory is scanned or written by the benchmark fixture.'

$previousEntries = [Environment]::GetEnvironmentVariable('IHUB_SEARCH_BENCH_ENTRIES', 'Process')
$previousSamples = [Environment]::GetEnvironmentVariable('IHUB_SEARCH_BENCH_SAMPLES', 'Process')
try {
    [Environment]::SetEnvironmentVariable('IHUB_SEARCH_BENCH_ENTRIES', [string]$Entries, 'Process')
    [Environment]::SetEnvironmentVariable('IHUB_SEARCH_BENCH_SAMPLES', [string]$Samples, 'Process')
    & $cargo.Path test --release --manifest-path $manifestPath --lib indexer::tests::local_search_performance_acceptance_benchmark -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) {
        throw "The local-search benchmark failed with exit code $LASTEXITCODE."
    }
}
finally {
    [Environment]::SetEnvironmentVariable('IHUB_SEARCH_BENCH_ENTRIES', $previousEntries, 'Process')
    [Environment]::SetEnvironmentVariable('IHUB_SEARCH_BENCH_SAMPLES', $previousSamples, 'Process')
}
