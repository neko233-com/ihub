#!/usr/bin/env bash
# Runs the ignored, metadata-only local-search acceptance benchmark. The Rust
# fixture never scans user roots, starts iHub, creates a watcher, or writes an
# index snapshot. Cargo may update its normal build/dependency caches, but
# never an iHub index root or user content.

set -euo pipefail

entries=100000
samples=21

usage() {
  printf '%s\n' \
    'Usage: bash scripts/run-search-benchmark.sh [--entries 100000|500000|1000000] [--samples ODD_5_TO_101]' \
    '' \
    'Runs the synthetic, in-memory local-search benchmark without scanning or writing user directories.'
}

die() {
  printf 'iHub local-search benchmark error: %s\n' "$*" >&2
  exit 1
}

while (($# > 0)); do
  case "$1" in
    --entries)
      (($# >= 2)) || die '--entries requires a value.'
      entries="$2"
      shift 2
      ;;
    --samples)
      (($# >= 2)) || die '--samples requires a value.'
      samples="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown argument: $1"
      ;;
  esac
done

case "$entries" in
  100000|500000|1000000) ;;
  *) die '--entries must be 100000, 500000, or 1000000.' ;;
esac

[[ "$samples" =~ ^[0-9]+$ ]] || die '--samples must be an integer.'
(( samples >= 5 && samples <= 101 && samples % 2 == 1 )) \
  || die '--samples must be an odd integer from 5 through 101.'

command -v cargo >/dev/null 2>&1 || die "Required command 'cargo' was not found on PATH."
script_directory="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
repository_root="$(CDPATH='' cd -- "$script_directory/.." && pwd)"
manifest_path="$repository_root/src-tauri/Cargo.toml"
[[ -f "$manifest_path" ]] || die "Could not find the iHub Cargo manifest: $manifest_path"

printf '%s\n' "Running iHub synthetic local-search benchmark: $entries input entries, $samples samples/query."
printf '%s\n' 'No user directory is scanned or written by the benchmark fixture.'

IHUB_SEARCH_BENCH_ENTRIES="$entries" \
IHUB_SEARCH_BENCH_SAMPLES="$samples" \
cargo test --release --manifest-path "$manifest_path" --lib indexer::tests::local_search_performance_acceptance_benchmark -- --ignored --nocapture
