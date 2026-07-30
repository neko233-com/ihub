#!/usr/bin/env bash
# Starts iHub directly from this checkout. It intentionally never changes Git
# state unless an explicitly requested safe-update mode is used.

set -euo pipefail

update=0
update_if_clean=0
skip_install=0
skip_check=0
build=0
package=0
install_latest=0
watch_install=0
watch_interval_seconds=2
watch_stop_signal_path=''
verify_only=0

usage() {
  printf '%s\n' \
    'Usage: bash scripts/dev.sh [options]' \
    '' \
    '  --update         Fetch and fast-forward only when the worktree is clean.' \
    '  --update-if-clean Attempt the same safe fast-forward before launch, but keep' \
    '                    running the current saved source when Git cannot update it.' \
    '  --skip-install   Do not run pnpm install --frozen-lockfile.' \
    '  --skip-check     Do not run pnpm check before the selected action.' \
    '  --build          Build the current source without an installer bundle.' \
    '  --package        Build native installer artifacts from the current source.' \
    '  --install-latest Build, validate, and install this exact current worktree' \
    '                   to the current-user ~/Applications directory.' \
    '  --watch-install  Explicitly watch saved source files and keep that local' \
    '                   developer installation current after stable changes.' \
    '  --watch-interval-seconds N  Poll interval for --watch-install (1-300; default: 2).' \
    '  --watch-stop-signal-path PATH  Optional literal file path. When it exists,' \
    '                   --watch-install exits cooperatively at a safe boundary.' \
    '  --verify-only    Verify prerequisites/dependencies and exit without launching.' \
    '  -h, --help       Show this help.'
}

die() {
  printf 'iHub developer launcher error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "Required command '$1' was not found on PATH."
}

run() {
  printf '+ '
  printf '%q ' "$@"
  printf '\n'
  "$@"
}

run_pnpm() {
  run corepack pnpm "$@"
}

watch_stop_requested() {
  [[ -n "$watch_stop_signal_path" ]] || return 1
  if [[ -L "$watch_stop_signal_path" ]]; then
    die "WatchInstall refuses a stop signal behind a symbolic link: $watch_stop_signal_path"
  fi
  if [[ -e "$watch_stop_signal_path" && ! -f "$watch_stop_signal_path" ]]; then
    die "WatchInstall stop signal must be a regular file when present: $watch_stop_signal_path"
  fi
  [[ -f "$watch_stop_signal_path" ]]
}

validate_macos_watch_stop_signal_path() {
  [[ -n "$watch_stop_signal_path" ]] || return 0
  [[ "$watch_stop_signal_path" == /* ]] || die '--watch-stop-signal-path must be an absolute, literal path below the current user home directory.'

  local parent="${watch_stop_signal_path%/*}"
  local base="${watch_stop_signal_path##*/}"
  [[ -n "$parent" ]] || parent='/'
  [[ -n "$base" && "$base" != '.' && "$base" != '..' ]] || die '--watch-stop-signal-path must name a regular file.'
  [[ -d "$parent" && ! -L "$parent" ]] || die "WatchInstall stop-signal parent must be an existing non-symlink directory: $parent"

  local resolved_parent
  resolved_parent="$(cd -- "$parent" && pwd -P)" || die "Could not resolve WatchInstall stop-signal parent: $parent"
  if [[ "$resolved_parent" != "$macos_developer_home" && "$resolved_parent" != "$macos_developer_home/"* ]]; then
    die '--watch-stop-signal-path must remain below the current user home directory.'
  fi
  watch_stop_signal_path="$resolved_parent/$base"

  if [[ -L "$watch_stop_signal_path" ]]; then
    die "WatchInstall refuses a stop signal behind a symbolic link: $watch_stop_signal_path"
  fi
  if [[ -e "$watch_stop_signal_path" && ! -f "$watch_stop_signal_path" ]]; then
    die "WatchInstall stop signal must be a regular file when present: $watch_stop_signal_path"
  fi
}

prepare_updater_signing() {
  if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    return
  fi

  local key_path="${IHUB_UPDATER_PRIVATE_KEY_PATH:-}"
  local password_path="${IHUB_UPDATER_PASSWORD_PATH:-}"
  if [[ -z "$key_path" ]]; then
    die '--package requires TAURI_SIGNING_PRIVATE_KEY. For a local key file, set IHUB_UPDATER_PRIVATE_KEY_PATH; a password is only needed for an encrypted key.'
  fi
  [[ -f "$key_path" ]] || die "Updater private key file is missing: $key_path"
  if [[ -n "$password_path" && ! -f "$password_path" ]]; then
    die "IHUB_UPDATER_PASSWORD_PATH was set but does not point to a password file: $password_path"
  fi

  # Tauri accepts a private-key file path as well as key contents. run() prints
  # argv only, so no signing material is emitted by this launcher.
  export TAURI_SIGNING_PRIVATE_KEY="$key_path"
  if [[ -n "$password_path" ]]; then
    # A password is optional: Tauri-generated keys may be unencrypted.
    local signing_password
    signing_password="$(<"$password_path")" || die "Could not read updater signing password file: $password_path"
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$signing_password"
  else
    unset TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  fi
}

load_macos_bundle_descriptor() {
  local config_path="$repository_root/src-tauri/tauri.conf.json"
  local values
  if ! values="$(IHUB_TAURI_CONFIG="$config_path" node - <<'NODE'
const fs = require('node:fs');
const configPath = process.env.IHUB_TAURI_CONFIG;
const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
const productName = String(config.productName ?? '');
const version = String(config.version ?? '');
const binaryName = String(config.mainBinaryName ?? '');
const safeProduct = /^[A-Za-z0-9][A-Za-z0-9 ._-]*$/;
const safeVersion = /^[0-9A-Za-z][0-9A-Za-z.+-]*$/;
const safeBinary = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
if (!safeProduct.test(productName) || !safeVersion.test(version) || !safeBinary.test(binaryName)) {
  throw new Error('Tauri productName, version, or mainBinaryName is not safe for a local macOS developer install path.');
}
process.stdout.write([productName, version, binaryName].join('\t'));
NODE
)"; then
    die "Could not parse a safe Tauri macOS bundle descriptor from $config_path"
  fi

  IFS=$'\t' read -r macos_product_name macos_version macos_binary_name <<< "$values"
  [[ -n "$macos_product_name" && -n "$macos_version" && -n "$macos_binary_name" ]] || die 'Tauri macOS bundle descriptor is incomplete.'
  macos_bundle_root="$repository_root/src-tauri/target/release/bundle/macos"
  macos_app_path="$macos_bundle_root/$macos_product_name.app"
  macos_updater_archive_path="$macos_bundle_root/$macos_product_name.app.tar.gz"
  macos_updater_signature_path="$macos_updater_archive_path.sig"
}

assert_macos_regular_file_or_missing() {
  local path="$1"
  local description="$2"
  if [[ -L "$path" ]]; then
    die "Refusing a $description behind a symbolic link: $path"
  fi
  if [[ -e "$path" && ! -f "$path" ]]; then
    die "Expected $description to be a regular file: $path"
  fi
}

clear_expected_macos_updater_artifacts() {
  # Tauri writes the macOS updater archive and sidecar independently. Clear
  # only these two descriptor-derived, non-symlink files before an explicit
  # development package so an old signature cannot satisfy a new build.
  for artifact in "$macos_updater_archive_path" "$macos_updater_signature_path"; do
    assert_macos_regular_file_or_missing "$artifact" 'macOS updater artifact'
    if [[ -e "$artifact" ]]; then
      /bin/rm -f "$artifact" || die "Could not clear previous macOS updater artifact: $artifact"
    fi
  done
}

macos_regular_file_state() {
  local path="$1"
  [[ -f "$path" && ! -L "$path" ]] || return 1
  /usr/bin/stat -f '%m:%z' "$path"
}

wait_for_current_macos_bundle() {
  local not_before="$1"
  local timeout_seconds=180
  local deadline=$(( $(/bin/date +%s) + timeout_seconds ))
  local previous_fingerprint=''
  local stable_observations=0

  while (( $(/bin/date +%s) < deadline )); do
    local archive_state=''
    local signature_state=''
    if [[ -d "$macos_app_path" && ! -L "$macos_app_path" ]] \
      && archive_state="$(macos_regular_file_state "$macos_updater_archive_path")" \
      && signature_state="$(macos_regular_file_state "$macos_updater_signature_path")"; then
      local archive_mtime="${archive_state%%:*}"
      local signature_mtime="${signature_state%%:*}"
      if (( archive_mtime >= not_before && signature_mtime >= not_before )); then
        local fingerprint="$archive_state:$signature_state"
        if [[ "$fingerprint" == "$previous_fingerprint" ]]; then
          stable_observations=$((stable_observations + 1))
          if (( stable_observations >= 2 )); then
            return 0
          fi
        else
          previous_fingerprint="$fingerprint"
          stable_observations=0
        fi
      fi
    fi
    /bin/sleep 1
  done

  die "Timed out waiting for a fresh signed macOS bundle. Expected '$macos_app_path', '$macos_updater_archive_path', and '$macos_updater_signature_path'. No developer app was installed."
}

build_current_signed_macos_bundle() {
  load_macos_bundle_descriptor
  local before_app_path="$macos_app_path"
  local before_archive_path="$macos_updater_archive_path"
  local before_signature_path="$macos_updater_signature_path"
  clear_expected_macos_updater_artifacts
  local packaging_started_at
  packaging_started_at="$(/bin/date +%s)"
  prepare_updater_signing
  run_pnpm tauri build

  load_macos_bundle_descriptor
  [[ "$before_app_path" == "$macos_app_path" && "$before_archive_path" == "$macos_updater_archive_path" && "$before_signature_path" == "$macos_updater_signature_path" ]] \
    || die 'Tauri macOS bundle identity changed while packaging. No developer app was installed; rerun after the configuration is stable.'
  wait_for_current_macos_bundle "$packaging_started_at"
}

load_macos_developer_install_target() {
  load_macos_bundle_descriptor
  [[ -n "${HOME:-}" && -d "$HOME" ]] || die 'HOME is unavailable; cannot validate the user-local macOS developer installation target.'
  local resolved_home
  resolved_home="$(cd -- "$HOME" && pwd -P)" || die 'Could not resolve HOME for the macOS developer installation target.'
  macos_developer_home="$resolved_home"
  macos_developer_applications_root="$resolved_home/Applications"
  macos_developer_destination="$macos_developer_applications_root/$macos_product_name.app"
  macos_developer_executable="$macos_developer_destination/Contents/MacOS/$macos_binary_name"

  if [[ -L "$macos_developer_applications_root" ]]; then
    die "Refusing to install through a symbolic-link Applications directory: $macos_developer_applications_root"
  fi
  if [[ -e "$macos_developer_applications_root" && ! -d "$macos_developer_applications_root" ]]; then
    die "The macOS developer Applications path is not a directory: $macos_developer_applications_root"
  fi
  if [[ -L "$macos_developer_destination" ]]; then
    die "Refusing to replace a symbolic-link iHub developer app: $macos_developer_destination"
  fi
  if [[ -e "$macos_developer_destination" && ! -d "$macos_developer_destination" ]]; then
    die "The macOS developer iHub destination is not an app directory: $macos_developer_destination"
  fi
}

load_macos_developer_install_lock_path() {
  # All developer worktrees for this user ultimately replace the same
  # ~/Applications/<product>.app destination.  Keep one narrow lock beside
  # the user-local persistent-service metadata so manual installs, the
  # terminal watcher, and a LaunchAgent cannot clear/build the same artifacts
  # or stage the same destination concurrently.
  load_macos_developer_install_target
  local support_parent="$macos_developer_home/Library/Application Support"
  local lock_parent="$support_parent/iHub Development"
  if [[ -L "$support_parent" ]]; then
    die "Refusing to create an iHub developer install lock through a symbolic-link Application Support directory: $support_parent"
  fi
  if [[ -e "$support_parent" && ! -d "$support_parent" ]]; then
    die "The macOS Application Support path is not a directory: $support_parent"
  fi
  /bin/mkdir -p "$lock_parent" || die "Could not create the user-local iHub developer lock directory: $lock_parent"
  [[ -d "$lock_parent" && ! -L "$lock_parent" ]] || die "Refusing to use an unsafe iHub developer lock directory: $lock_parent"
  macos_developer_install_lock_path="$lock_parent/.${macos_product_name}.install.lock"
}

macos_developer_install_lock_path=''
macos_developer_install_lock_held=0

release_macos_developer_install_lock() {
  (( macos_developer_install_lock_held == 1 )) || return 0

  local owner_path="$macos_developer_install_lock_path/owner.pid"
  if [[ -d "$macos_developer_install_lock_path" && ! -L "$macos_developer_install_lock_path" && -f "$owner_path" && ! -L "$owner_path" ]]; then
    local owner_pid=''
    IFS= read -r owner_pid < "$owner_path" || true
    if [[ "$owner_pid" == "$$" ]]; then
      /bin/rm -f "$owner_path" || true
      /bin/rmdir "$macos_developer_install_lock_path" 2>/dev/null || true
    else
      printf 'Warning: iHub developer install lock ownership changed unexpectedly; it was left in place: %s\n' "$macos_developer_install_lock_path" >&2
    fi
  fi
  macos_developer_install_lock_held=0
}

acquire_macos_developer_install_lock() {
  load_macos_developer_install_lock_path
  local owner_path="$macos_developer_install_lock_path/owner.pid"

  if /bin/mkdir "$macos_developer_install_lock_path" 2>/dev/null; then
    printf '%s\n' "$$" > "$owner_path" || {
      /bin/rmdir "$macos_developer_install_lock_path" 2>/dev/null || true
      die "Could not record iHub developer install-lock ownership: $owner_path"
    }
    macos_developer_install_lock_held=1
    return 0
  fi

  [[ -d "$macos_developer_install_lock_path" && ! -L "$macos_developer_install_lock_path" ]] \
    || die "Another iHub developer installation lock exists at an unsafe path: $macos_developer_install_lock_path"
  [[ -f "$owner_path" && ! -L "$owner_path" ]] \
    || die "Another iHub developer installation is active or its lock is incomplete: $macos_developer_install_lock_path"

  local owner_pid=''
  IFS= read -r owner_pid < "$owner_path" || true
  [[ "$owner_pid" =~ ^[0-9]+$ ]] \
    || die "Another iHub developer installation lock has an invalid owner record: $macos_developer_install_lock_path"
  if /bin/ps -p "$owner_pid" -o pid= >/dev/null 2>&1; then
    die "Another iHub developer packaging/install operation is active (PID $owner_pid). It will not be interrupted; retry after it exits."
  fi

  # A crashed owner can leave the one-file lock behind.  Only reclaim this
  # exact, empty-after-owner directory; never recurse or remove a directory
  # containing unexpected data.
  local entry_count
  entry_count="$(/usr/bin/find "$macos_developer_install_lock_path" -mindepth 1 -maxdepth 1 -print | /usr/bin/wc -l | /usr/bin/tr -d '[:space:]')"
  [[ "$entry_count" == '1' ]] \
    || die "A stale-looking iHub developer install lock contains unexpected data and was left untouched: $macos_developer_install_lock_path"
  /bin/rm -f "$owner_path" || die "Could not clear the stale iHub developer install-lock owner record: $owner_path"
  /bin/rmdir "$macos_developer_install_lock_path" || die "Could not clear the stale iHub developer install lock: $macos_developer_install_lock_path"
  acquire_macos_developer_install_lock
}

run_macos_developer_install_critical() {
  # A separate subshell lets a watcher keep running after a single build or
  # install failure.  Its EXIT trap releases only the lock it acquired; it
  # never touches iHub itself or any other process.
  (
    acquire_macos_developer_install_lock
    trap 'release_macos_developer_install_lock' EXIT
    "$@"
  )
}

get_running_macos_ihub_pids() {
  local output
  local status
  if output="$(/usr/bin/pgrep -x "$macos_binary_name" 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  else
    status=$?
  fi
  if (( status == 1 )); then
    return 1
  fi
  die "Could not inspect running iHub processes before a macOS developer installation."
}

assert_macos_developer_target_not_running() {
  local pids
  if pids="$(get_running_macos_ihub_pids)"; then
    die "An iHub process is running (PID(s) ${pids//$'\n'/, }). Close it yourself, then rerun --install-latest or keep --watch-install open. The script never stops processes."
  fi
}

install_current_macos_bundle() {
  load_macos_developer_install_target
  assert_macos_developer_target_not_running
  [[ -d "$macos_app_path" && ! -L "$macos_app_path" ]] || die "The fresh Tauri macOS app bundle is unavailable: $macos_app_path"
  [[ -f "$macos_app_path/Contents/MacOS/$macos_binary_name" && ! -L "$macos_app_path/Contents/MacOS/$macos_binary_name" ]] \
    || die "The fresh Tauri macOS app bundle has no regular main executable: $macos_app_path/Contents/MacOS/$macos_binary_name"

  /bin/mkdir -p "$macos_developer_applications_root" || die "Could not create the user-local Applications directory: $macos_developer_applications_root"
  [[ -d "$macos_developer_applications_root" && ! -L "$macos_developer_applications_root" ]] \
    || die "Refusing to install through an unsafe user-local Applications directory: $macos_developer_applications_root"

  local staging="$macos_developer_applications_root/.$macos_product_name.installing.$$"
  local previous="$macos_developer_applications_root/.$macos_product_name.previous.$$"
  [[ ! -e "$staging" && ! -L "$staging" && ! -e "$previous" && ! -L "$previous" ]] \
    || die 'A developer installation staging path already exists. Refusing to overwrite it.'

  /usr/bin/ditto "$macos_app_path" "$staging" || die "Could not stage the fresh iHub macOS app bundle at $staging"
  [[ -f "$staging/Contents/MacOS/$macos_binary_name" && ! -L "$staging/Contents/MacOS/$macos_binary_name" ]] \
    || die "The staged iHub macOS app bundle is incomplete: $staging"

  local moved_previous=0
  if [[ -e "$macos_developer_destination" ]]; then
    [[ -d "$macos_developer_destination" && ! -L "$macos_developer_destination" ]] \
      || die "Refusing to replace an unsafe iHub developer destination: $macos_developer_destination"
    /bin/mv "$macos_developer_destination" "$previous" || die "Could not preserve the previous iHub developer app at $previous"
    moved_previous=1
  fi

  if ! /bin/mv "$staging" "$macos_developer_destination"; then
    if (( moved_previous == 1 )); then
      /bin/mv "$previous" "$macos_developer_destination" || true
    fi
    die 'Could not move the fresh iHub developer app into place.'
  fi
  [[ -f "$macos_developer_executable" && ! -L "$macos_developer_executable" ]] \
    || die "The installed iHub developer app has no regular main executable: $macos_developer_executable"

  if (( moved_previous == 1 )); then
    [[ -d "$previous" && ! -L "$previous" ]] || die "Refusing to remove an unsafe previous iHub developer app: $previous"
    /bin/rm -rf "$previous" || die "The fresh iHub developer app is installed, but the previous app could not be removed: $previous"
  fi

  printf 'Installed the current macOS worktree to %s\n' "$macos_developer_destination"
  printf '%s\n' 'No iHub process was stopped or launched. Open the developer app yourself when ready.'
}

install_latest_current_worktree_macos_unlocked() {
  load_macos_developer_install_target
  assert_macos_developer_target_not_running
  build_current_signed_macos_bundle
  install_current_macos_bundle
}

install_latest_current_worktree_macos() {
  run_macos_developer_install_critical install_latest_current_worktree_macos_unlocked
}

package_current_worktree_macos() {
  run_macos_developer_install_critical build_current_signed_macos_bundle
}

get_development_source_fingerprint() {
  # This snapshot deliberately uses path, size, and nanosecond mtime metadata
  # instead of reading every file. It is only a debounce trigger for an
  # explicitly requested local build; package signature validation remains
  # the build/install path's responsibility. Node is already a prerequisite
  # for this launcher and keeps a large Git-visible worktree inexpensive to
  # inspect on macOS.
  IHUB_DEVELOPMENT_SOURCE_ROOT="$repository_root" node - <<'NODE'
const { execFileSync } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const configuredRoot = process.env.IHUB_DEVELOPMENT_SOURCE_ROOT;
if (!configuredRoot) {
  throw new Error('IHUB_DEVELOPMENT_SOURCE_ROOT is unavailable.');
}
const root = fs.realpathSync.native(configuredRoot);
const watchedPaths = execFileSync(
  'git',
  ['-C', root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard'],
  {
    encoding: 'buffer',
    stdio: ['ignore', 'pipe', 'inherit'],
    windowsHide: true,
  },
)
  .toString('utf8')
  .split('\0')
  .filter(Boolean)
  .sort();
const fingerprint = crypto.createHash('sha256');
fingerprint.update('ihub-development-watch-v1\n');

for (const relativePath of watchedPaths) {
  if (
    path.isAbsolute(relativePath) ||
    relativePath.split(/[\\/]/).some((segment) => segment === '' || segment === '.' || segment === '..')
  ) {
    throw new Error(`Unsafe watched source path: ${relativePath}`);
  }

  const fullPath = path.resolve(root, relativePath);
  const pathFromRoot = path.relative(root, fullPath);
  if (
    pathFromRoot === '' ||
    pathFromRoot === '..' ||
    pathFromRoot.startsWith(`..${path.sep}`) ||
    path.isAbsolute(pathFromRoot)
  ) {
    throw new Error(`Watched source path escapes the worktree: ${relativePath}`);
  }

  let entry;
  try {
    entry = fs.lstatSync(fullPath, { bigint: true });
  } catch (error) {
    if (error && error.code === 'ENOENT') {
      fingerprint.update(relativePath);
      fingerprint.update('\0missing\n');
      continue;
    }
    throw error;
  }
  if (entry.isSymbolicLink()) {
    throw new Error(`WatchInstall refuses a source path behind a symbolic link: ${fullPath}`);
  }

  fingerprint.update(relativePath);
  fingerprint.update('\0');
  if (entry.isDirectory()) {
    // Nested official plugins are independent Git worktrees and are not
    // bundled into the root desktop app. Keep a stable boundary instead of
    // recursively scanning another repository (or its dependencies).
    fingerprint.update('directory-boundary\n');
  } else if (entry.isFile()) {
    fingerprint.update(`${entry.size}:${entry.mtimeNs}\n`);
  } else {
    throw new Error(`WatchInstall only accepts regular source files or directories: ${fullPath}`);
  }
}

process.stdout.write(`${fingerprint.digest('hex')}\n`);
NODE
}

watch_install_current_worktree_macos() {
  if watch_stop_requested; then
    printf '%s\n' 'WatchInstall observed the requested stop signal before starting. No process was stopped or launched.'
    return 0
  fi
  printf 'WatchInstall is monitoring saved iHub source files every %s second(s). Press Ctrl+C to stop.\n' "$watch_interval_seconds"
  printf '%s\n' 'It does not fetch, pull, reset, checkout, clean, launch iHub, or stop any process. A stop signal is observed only at safe watcher/build boundaries.'

  local last_observed=''
  local stable_observations=0
  local last_installed=''
  local last_failed=''
  local last_blocked_pids=''

  while :; do
    if watch_stop_requested; then
      printf '%s\n' 'WatchInstall observed the requested stop signal at a safe boundary. No process was stopped or launched.'
      return 0
    fi

    local fingerprint
    fingerprint="$(get_development_source_fingerprint)" || die 'Could not fingerprint the saved iHub source files for --watch-install.'
    if [[ "$fingerprint" == "$last_observed" ]]; then
      stable_observations=$((stable_observations + 1))
    else
      last_observed="$fingerprint"
      stable_observations=0
      last_failed=''
      printf 'Detected saved source change (%s); waiting for one stable poll before packaging.\n' "${fingerprint:0:12}"
    fi

    if [[ "$fingerprint" != "$last_installed" && "$stable_observations" -ge 1 && "$fingerprint" != "$last_failed" ]]; then
      load_macos_developer_install_target
      local pids=''
      if pids="$(get_running_macos_ihub_pids)"; then
        if [[ "$pids" != "$last_blocked_pids" ]]; then
          printf 'Warning: WatchInstall is waiting for iHub PID(s) %s to close. No process will be stopped.\n' "${pids//$'\n'/, }" >&2
        fi
        last_blocked_pids="$pids"
      else
        last_blocked_pids=''
        if ( install_latest_current_worktree_macos ); then
          last_installed="$fingerprint"
          last_failed=''
          printf 'WatchInstall completed for source snapshot %s.\n' "${fingerprint:0:12}"
        else
          load_macos_developer_install_target
          if pids="$(get_running_macos_ihub_pids)"; then
            last_blocked_pids="$pids"
            printf '%s\n' 'Warning: iHub opened while WatchInstall was packaging; installation was not forced. Close it to allow a retry.' >&2
          else
            last_failed="$fingerprint"
            printf 'Warning: WatchInstall failed for source snapshot %s. Save another source change after fixing the problem to retry.\n' "${fingerprint:0:12}" >&2
          fi
        fi
      fi
    fi

    if watch_stop_requested; then
      printf '%s\n' 'WatchInstall observed the requested stop signal before another packaging attempt. No process was stopped or launched.'
      return 0
    fi
    /bin/sleep "$watch_interval_seconds"
  done
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --update)
      update=1
      shift
      ;;
    --update-if-clean)
      update_if_clean=1
      shift
      ;;
    --skip-install)
      skip_install=1
      shift
      ;;
    --skip-check)
      skip_check=1
      shift
      ;;
    --build)
      build=1
      shift
      ;;
    --package)
      package=1
      shift
      ;;
    --install-latest)
      install_latest=1
      shift
      ;;
    --watch-install)
      watch_install=1
      shift
      ;;
    --watch-interval-seconds)
      [[ $# -ge 2 ]] || die '--watch-interval-seconds requires a value from 1 to 300.'
      watch_interval_seconds="$2"
      shift 2
      ;;
    --watch-stop-signal-path)
      [[ $# -ge 2 ]] || die '--watch-stop-signal-path requires an absolute literal file path.'
      watch_stop_signal_path="$2"
      shift 2
      ;;
    --verify-only)
      verify_only=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown option: $1"
      ;;
  esac
done

if ! [[ "$watch_interval_seconds" =~ ^[0-9]+$ ]] || (( watch_interval_seconds < 1 || watch_interval_seconds > 300 )); then
  die '--watch-interval-seconds must be an integer from 1 to 300.'
fi
mode_count=$((build + package + install_latest + watch_install + verify_only))
[[ "$mode_count" -le 1 ]] || die 'Use only one of --build, --package, --install-latest, --watch-install, or --verify-only.'
[[ -z "$watch_stop_signal_path" || "$watch_install" == '1' ]] || die '--watch-stop-signal-path is valid only with --watch-install.'
(( update == 0 || update_if_clean == 0 )) || die 'Use either --update for strict behavior or --update-if-clean for best-effort safe behavior, not both.'
(( (install_latest == 0 && watch_install == 0) || (update == 0 && update_if_clean == 0) )) || die '--install-latest and --watch-install never update Git. Run an update mode separately, review it, then start the local installation mode.'
[[ "$(/usr/bin/uname -s)" == 'Darwin' ]] || die 'scripts/dev.sh is for macOS. Use scripts/dev.ps1 on Windows.'

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repository_root="$(cd -- "$script_dir/.." && pwd -P)"
cd -- "$repository_root"

for required_file in package.json pnpm-lock.yaml src-tauri/tauri.conf.json; do
  [[ -f "$required_file" ]] || die "This does not look like an iHub checkout: missing $required_file."
done

for required_command in git node corepack cargo; do
  require_command "$required_command"
done
if (( install_latest == 1 || watch_install == 1 )); then
  for required_command in ditto pgrep stat; do
    require_command "$required_command"
  done
fi

[[ "$(git rev-parse --is-inside-work-tree)" == 'true' ]] || die 'scripts/dev.sh must run from an iHub Git worktree.'

if [[ -n "$watch_stop_signal_path" ]]; then
  # Resolve HOME and the exact developer target before accepting a cooperative
  # stop file.  The watcher only reads this literal, regular file; it never
  # removes it or treats it as a command.
  load_macos_developer_install_target
  validate_macos_watch_stop_signal_path
fi

node_version="$(node --version)"
node_semver="${node_version#v}"
IFS='.' read -r node_major node_minor node_patch <<< "$node_semver"
[[ "$node_major" =~ ^[0-9]+$ && "$node_minor" =~ ^[0-9]+$ && "$node_patch" =~ ^[0-9]+$ ]] || die "Could not parse Node.js version '$node_version'."
(( node_major > 22 || (node_major == 22 && node_minor >= 12) )) || die "iHub requires Node.js 22.12 or newer; found $node_version."

run corepack pnpm --version

worktree_changes="$(git status --porcelain=v1 --untracked-files=normal)"
safe_update_skip() {
  local message="$1"
  if (( update_if_clean == 1 )); then
    printf 'Warning: safe update skipped: %s No working-tree files were changed by iHub.\n' "$message" >&2
    printf '%s\n' 'Warning: continuing with the current saved source.' >&2
    return 0
  fi
  die "$message"
}

safe_fast_forward() {
  if [[ -n "$worktree_changes" ]]; then
    safe_update_skip 'The worktree is dirty. No fetch, merge, reset, checkout, or clean operation was performed.'
    return
  fi

  local upstream
  if ! upstream="$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null)"; then
    safe_update_skip 'The current branch has no upstream.'
    return
  fi
  if [[ -z "$upstream" ]]; then
    safe_update_skip 'The current branch has no upstream.'
    return
  fi
  if ! run git fetch --prune; then
    safe_update_skip "Could not fetch $upstream."
    return
  fi

  local counts ahead behind
  if ! counts="$(git rev-list --left-right --count "HEAD...$upstream")"; then
    safe_update_skip "Could not determine divergence from $upstream."
    return
  fi
  read -r ahead behind <<< "$counts"
  if [[ ! "$ahead" =~ ^[0-9]+$ || ! "$behind" =~ ^[0-9]+$ ]]; then
    safe_update_skip "Could not determine divergence from $upstream."
    return
  fi
  if (( ahead > 0 && behind > 0 )); then
    safe_update_skip "Local branch has diverged from $upstream ($ahead ahead, $behind behind)."
    return
  fi
  if (( ahead > 0 )); then
    safe_update_skip "Local branch is $ahead commit(s) ahead of $upstream."
    return
  fi
  if (( behind > 0 )); then
    printf 'Fast-forwarding %s commit(s) from %s...\n' "$behind" "$upstream"
    if ! run git merge --ff-only "$upstream"; then
      safe_update_skip "Could not fast-forward from $upstream."
    fi
  else
    printf 'Source is already current with %s.\n' "$upstream"
  fi
}

if (( update == 1 || update_if_clean == 1 )); then
  safe_fast_forward
elif [[ -n "$worktree_changes" ]]; then
  printf '%s\n' 'Warning: starting from the current dirty worktree. No fetch, pull, reset, checkout, or clean operation is performed.' >&2
else
  printf '%s\n' 'Starting from the currently checked-out source. Use --update for a strict fast-forward or --update-if-clean to follow upstream when safe.'
fi

official_plugin_sync_mode='--locked'
if (( update == 1 )); then
  official_plugin_sync_mode='--update'
elif (( update_if_clean == 1 )); then
  official_plugin_sync_mode='--update-if-clean'
fi
printf 'Preparing independent official plugin checkouts (%s)...\n' "$official_plugin_sync_mode"
run node scripts/bootstrap-official-plugins.mjs "$official_plugin_sync_mode"

if [[ "$skip_install" == '0' ]]; then
  printf '%s\n' 'Synchronizing dependencies from pnpm-lock.yaml (frozen; package versions are not upgraded)...'
  run_pnpm install --frozen-lockfile
else
  printf '%s\n' 'Warning: skipping dependency synchronization by request.' >&2
fi

if [[ "$skip_check" == '0' ]]; then
  printf '%s\n' 'Checking TypeScript before launch...'
  run_pnpm check
else
  printf '%s\n' 'Warning: skipping TypeScript check by request.' >&2
fi

if [[ "$verify_only" == '1' ]]; then
  printf '%s\n' 'Development environment verification completed. No app was launched.'
  exit 0
fi

if [[ "$install_latest" == '1' ]]; then
  printf '%s\n' 'Building and installing a fresh signed macOS app from the current worktree...'
  install_latest_current_worktree_macos
  exit 0
fi

if [[ "$watch_install" == '1' ]]; then
  watch_install_current_worktree_macos
  exit 0
fi

if [[ "$build" == '1' ]]; then
  printf '%s\n' 'Building the current source without an installer bundle...'
  run_pnpm tauri build --no-bundle
  exit 0
fi

if [[ "$package" == '1' ]]; then
  printf '%s\n' 'Building native installer artifacts from the current source...'
  package_current_worktree_macos
  printf 'Installer artifacts are under %s\n' "$repository_root/src-tauri/target/release/bundle"
  exit 0
fi

printf '%s\n' 'Launching iHub from the current source. Tauri/Vite will rebuild and reload as files change.'
run_pnpm tauri dev
