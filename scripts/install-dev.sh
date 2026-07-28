#!/usr/bin/env bash
# Explicit macOS developer launcher and persistent-install service setup.
# This script never launches iHub.  The optional LaunchAgents are current-user
# only and are deliberately disabled unless the developer explicitly enables
# them after reviewing the source worktree and local signing-key path.

set -euo pipefail

mode=''
dry_run=0
upstream_check_minutes=30
signing_key_path=''
signing_password_path=''

launcher_owner='iHub macOS Development Launcher'
launcher_revision='1'
persistent_owner='iHub macOS Development persistent install service v1'
watch_label='com.neko233.ihub.development.watch'
refresh_label='com.neko233.ihub.development.refresh'

usage() {
  printf '%s\n' \
    'Usage: bash scripts/install-dev.sh <mode> [options]' \
    '' \
    '  --install-launcher  Create or refresh the user-local development launcher marker.' \
    '  --enable-persistent-development-install' \
    '                      Explicitly register current-user macOS LaunchAgents for' \
    '                      safe upstream refresh and local WatchInstall.' \
    '  --disable-persistent-development-install' \
    '                      Request cooperative shutdown and prevent future logins' \
    '                      from loading iHub-owned LaunchAgents.' \
    '  --development-install-status' \
    '                      Read launcher, plist, wrapper-status and launchd state.' \
    '' \
    'Options:' \
    '  --signing-key-path PATH      Required when enabling. Regular local updater' \
    '                               private-key file; its contents are never copied.' \
    '  --signing-password-path PATH Optional regular password-file path for an' \
    '                               encrypted local updater key.' \
    '  --upstream-check-minutes N   Refresh cadence, 10-240 (default: 30).' \
    '  --dry-run                    Describe filesystem/launchctl changes only.' \
    '  -h, --help                   Show this help.' \
    '' \
    'Normal launcher setup does not create a LaunchAgent. The persistent service' \
    'never uses sudo, open, a shell -c action, or an iHub app executable action.'
}

die() {
  printf 'iHub macOS developer launcher error: %s\n' "$*" >&2
  exit 1
}

path_exists_any() {
  [[ -e "$1" || -L "$1" ]]
}

assert_no_line_breaks() {
  local value="$1"
  local description="$2"
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] || die "$description must not contain a line break."
}

absolute_command_path() {
  local command_name="$1"
  local command_path
  command_path="$(command -v "$command_name" 2>/dev/null || true)"
  [[ "$command_path" == /* && -f "$command_path" && -x "$command_path" ]] \
    || die "Required executable '$command_name' is unavailable as an absolute executable path."
  local command_parent="${command_path%/*}"
  local command_base="${command_path##*/}"
  local resolved_parent
  resolved_parent="$(cd -- "$command_parent" && pwd -P)" || die "Could not resolve executable path for '$command_name'."
  printf '%s/%s\n' "$resolved_parent" "$command_base"
}

canonicalize_existing_regular_file() {
  local candidate="$1"
  local description="$2"
  [[ "$candidate" == /* ]] || die "$description must be an absolute path."
  local parent="${candidate%/*}"
  local base="${candidate##*/}"
  [[ -n "$parent" ]] || parent='/'
  [[ -n "$base" && "$base" != '.' && "$base" != '..' ]] || die "$description must name a regular file."
  [[ -d "$parent" ]] || die "$description parent directory is unavailable: $parent"
  local resolved_parent
  resolved_parent="$(cd -- "$parent" && pwd -P)" || die "Could not resolve $description parent: $parent"
  local resolved_path="$resolved_parent/$base"
  [[ -f "$resolved_path" && ! -L "$resolved_path" ]] || die "$description must be an existing regular, non-symlink file: $candidate"
  assert_no_line_breaks "$resolved_path" "$description"
  printf '%s\n' "$resolved_path"
}

canonicalize_existing_directory() {
  local candidate="$1"
  local description="$2"
  [[ "$candidate" == /* ]] || die "$description must be an absolute path."
  [[ -d "$candidate" && ! -L "$candidate" ]] || die "$description must be an existing non-symlink directory: $candidate"
  local resolved_path
  resolved_path="$(cd -- "$candidate" && pwd -P)" || die "Could not resolve $description: $candidate"
  assert_no_line_breaks "$resolved_path" "$description"
  printf '%s\n' "$resolved_path"
}

write_atomic_file() {
  local destination="$1"
  local content="$2"
  local mode_value="$3"
  local parent="${destination%/*}"
  local base="${destination##*/}"
  [[ -d "$parent" && ! -L "$parent" ]] || die "Refusing to write through an unsafe parent directory: $parent"
  if [[ -L "$destination" || ( -e "$destination" && ! -f "$destination" ) ]]; then
    die "Refusing to replace an unsafe file path: $destination"
  fi
  local temporary="$parent/.${base}.$$.${RANDOM}.tmp"
  if ! (
    umask 077
    printf '%s' "$content" > "$temporary" || exit 1
    /bin/chmod "$mode_value" "$temporary" || exit 1
    /bin/mv "$temporary" "$destination" || exit 1
  ); then
    /bin/rm -f "$temporary" 2>/dev/null || true
    die "Could not write $destination atomically."
  fi
}

write_atomic_plist() {
  local destination="$1"
  local content="$2"
  local parent="${destination%/*}"
  local base="${destination##*/}"
  [[ -d "$parent" && ! -L "$parent" ]] || die "Refusing to write a plist through an unsafe parent directory: $parent"
  if [[ -L "$destination" || ( -e "$destination" && ! -f "$destination" ) ]]; then
    die "Refusing to replace an unsafe plist path: $destination"
  fi
  local temporary="$parent/.${base}.$$.${RANDOM}.tmp"
  if ! (
    umask 077
    printf '%s' "$content" > "$temporary" || exit 1
    /usr/bin/plutil -lint "$temporary" >/dev/null || exit 1
    /bin/chmod 600 "$temporary" || exit 1
    /bin/mv "$temporary" "$destination" || exit 1
  ); then
    /bin/rm -f "$temporary" 2>/dev/null || true
    die "Could not write a valid plist at $destination."
  fi
}

shell_quote() {
  # The generated wrappers run under Bash, whose %q representation is a literal
  # shell word. This keeps source/key paths data rather than executable shell.
  printf '%q' "$1"
}

xml_escape() {
  IHUB_XML_VALUE="$1" "$node_bin" -e '
const value = process.env.IHUB_XML_VALUE;
if (typeof value !== "string" || /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(value)) process.exit(2);
process.stdout.write(value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/\x27/g, "&apos;"));
'
}

marker_field() {
  local field_name="$1"
  IHUB_MARKER_PATH="$launcher_marker_path" IHUB_MARKER_FIELD="$field_name" "$node_bin" -e '
const fs = require("node:fs");
const marker = JSON.parse(fs.readFileSync(process.env.IHUB_MARKER_PATH, "utf8"));
const field = process.env.IHUB_MARKER_FIELD;
const value = marker[field];
if (value === undefined || value === null) process.exit(2);
process.stdout.write(String(value));
'
}

launcher_marker_is_owned() {
  [[ -f "$launcher_marker_path" && ! -L "$launcher_marker_path" ]] || return 1
  local managed_by revision source_root
  managed_by="$(marker_field managedBy 2>/dev/null || true)"
  revision="$(marker_field launcherRevision 2>/dev/null || true)"
  source_root="$(marker_field sourceRoot 2>/dev/null || true)"
  [[ "$managed_by" == "$launcher_owner" && "$revision" == "$launcher_revision" && -n "$source_root" ]] || return 1
  [[ "$source_root" != *$'\n'* && "$source_root" != *$'\r'* ]] || return 1
  launcher_marker_source_root="$source_root"
  return 0
}

assert_owned_launcher_marker() {
  launcher_marker_is_owned \
    || die "The iHub macOS development launcher marker is missing, unsafe, or not trusted: $launcher_marker_path"
}

assert_launcher_targets_current_worktree() {
  assert_owned_launcher_marker
  local configured_root
  configured_root="$(canonicalize_existing_directory "$launcher_marker_source_root" 'Configured development source root')"
  [[ "$configured_root" == "$repository_root" ]] \
    || die "The trusted launcher points to '$configured_root', not this worktree '$repository_root'. Run --install-launcher from the intended worktree first."
}

plist_field() {
  local plist_path="$1"
  local field_name="$2"
  /usr/libexec/PlistBuddy -c "Print :$field_name" "$plist_path" 2>/dev/null
}

persistent_service_marker_field() {
  local field_name="$1"
  IHUB_SERVICE_MARKER_PATH="$persistent_service_marker_path" IHUB_SERVICE_MARKER_FIELD="$field_name" "$node_bin" -e '
const fs = require("node:fs");
const marker = JSON.parse(fs.readFileSync(process.env.IHUB_SERVICE_MARKER_PATH, "utf8"));
const field = process.env.IHUB_SERVICE_MARKER_FIELD;
const value = marker[field];
if (value === undefined || value === null) process.exit(2);
process.stdout.write(String(value));
'
}

persistent_service_marker_is_owned() {
  [[ -f "$persistent_service_marker_path" && ! -L "$persistent_service_marker_path" ]] || return 1
  local managed_by revision source_root marker_watch_label marker_refresh_label marker_watch_wrapper marker_refresh_wrapper
  managed_by="$(persistent_service_marker_field managedBy 2>/dev/null || true)"
  revision="$(persistent_service_marker_field serviceRevision 2>/dev/null || true)"
  source_root="$(persistent_service_marker_field sourceRoot 2>/dev/null || true)"
  marker_watch_label="$(persistent_service_marker_field watchLabel 2>/dev/null || true)"
  marker_refresh_label="$(persistent_service_marker_field refreshLabel 2>/dev/null || true)"
  marker_watch_wrapper="$(persistent_service_marker_field watchWrapperPath 2>/dev/null || true)"
  marker_refresh_wrapper="$(persistent_service_marker_field refreshWrapperPath 2>/dev/null || true)"
  [[ "$managed_by" == "$persistent_owner" && "$revision" == '1' && -n "$source_root" && "$marker_watch_label" == "$watch_label" && "$marker_refresh_label" == "$refresh_label" && "$marker_watch_wrapper" == "$watch_wrapper_path" && "$marker_refresh_wrapper" == "$refresh_wrapper_path" ]] || return 1
  [[ "$source_root" != *$'\n'* && "$source_root" != *$'\r'* ]] || return 1
  persistent_service_marker_source_root="$source_root"
  return 0
}

expected_wrapper_for_label() {
  case "$1" in
    "$watch_label") printf '%s\n' "$watch_wrapper_path" ;;
    "$refresh_label") printf '%s\n' "$refresh_wrapper_path" ;;
    *) return 1 ;;
  esac
}

persistent_plist_is_owned() {
  local plist_path="$1"
  local expected_label="$2"
  [[ -f "$plist_path" && ! -L "$plist_path" ]] || return 1
  persistent_service_marker_is_owned || return 1
  local expected_wrapper label argument_zero argument_one wrapper_marker
  expected_wrapper="$(expected_wrapper_for_label "$expected_label" || true)"
  [[ -n "$expected_wrapper" && -f "$expected_wrapper" && ! -L "$expected_wrapper" ]] || return 1
  label="$(plist_field "$plist_path" Label || true)"
  argument_zero="$(plist_field "$plist_path" 'ProgramArguments:0' || true)"
  argument_one="$(plist_field "$plist_path" 'ProgramArguments:1' || true)"
  wrapper_marker="$(/usr/bin/sed -n '2p' "$expected_wrapper" 2>/dev/null || true)"
  [[ "$label" == "$expected_label" && "$argument_zero" == '/bin/bash' && "$argument_one" == "$expected_wrapper" && "$wrapper_marker" == "# managedBy: $persistent_owner" ]]
}

assert_owned_persistent_plist() {
  local plist_path="$1"
  local expected_label="$2"
  persistent_plist_is_owned "$plist_path" "$expected_label" \
    || die "Refusing to change '$plist_path' because it is missing, unsafe, or lacks the iHub macOS persistent-service ownership marker."
}

assert_plist_is_absent_or_owned() {
  local plist_path="$1"
  local expected_label="$2"
  if path_exists_any "$plist_path"; then
    assert_owned_persistent_plist "$plist_path" "$expected_label"
  fi
}

assert_wrapper_is_absent_or_owned() {
  local wrapper_path="$1"
  if ! path_exists_any "$wrapper_path"; then
    return 0
  fi
  [[ -f "$wrapper_path" && ! -L "$wrapper_path" ]] \
    || die "Refusing to replace an unsafe persistent-service wrapper: $wrapper_path"
  local wrapper_marker
  wrapper_marker="$(/usr/bin/sed -n '2p' "$wrapper_path" 2>/dev/null || true)"
  [[ "$wrapper_marker" == "# managedBy: $persistent_owner" ]] \
    || die "Refusing to replace a wrapper without the iHub persistent-service ownership marker: $wrapper_path"
}

launch_agent_is_loaded() {
  local label="$1"
  /bin/launchctl print "gui/$current_user_id/$label" >/dev/null 2>&1
}

ensure_support_root_for_launcher() {
  if path_exists_any "$support_root"; then
    [[ -d "$support_root" && ! -L "$support_root" ]] \
      || die "iHub macOS development support path is unsafe: $support_root"
    if [[ ! -f "$launcher_marker_path" ]]; then
      # scripts/dev.sh may have created only its narrow install lock before a
      # launcher is first installed. Do not adopt any other pre-existing data.
      local unexpected
      unexpected="$(/usr/bin/find "$support_root" -mindepth 1 -maxdepth 1 ! -name '.*.install.lock' -print -quit)"
      [[ -z "$unexpected" ]] || die "Refusing to adopt an existing non-iHub development support directory: $support_root"
    elif [[ -L "$launcher_marker_path" ]]; then
      die "iHub macOS development launcher marker is a symbolic link: $launcher_marker_path"
    fi
    return 0
  fi
  if (( dry_run == 1 )); then
    printf '[dry-run] create user-local support directory: %s\n' "$support_root"
    return 0
  fi
  /bin/mkdir -p "$support_root" || die "Could not create iHub macOS development support directory: $support_root"
  /bin/chmod 700 "$support_root" || die "Could not protect iHub macOS development support directory: $support_root"
}

ensure_launch_agents_root() {
  if path_exists_any "$launch_agents_root"; then
    [[ -d "$launch_agents_root" && ! -L "$launch_agents_root" ]] \
      || die "macOS LaunchAgents directory is unsafe: $launch_agents_root"
    return 0
  fi
  if (( dry_run == 1 )); then
    printf '[dry-run] create user LaunchAgents directory: %s\n' "$launch_agents_root"
    return 0
  fi
  /bin/mkdir -p "$launch_agents_root" || die "Could not create user LaunchAgents directory: $launch_agents_root"
  /bin/chmod 700 "$launch_agents_root" || die "Could not protect user LaunchAgents directory: $launch_agents_root"
}

write_launcher_marker() {
  local marker_content
  marker_content="$(IHUB_SOURCE_ROOT="$repository_root" IHUB_LAUNCHER_OWNER="$launcher_owner" IHUB_LAUNCHER_REVISION="$launcher_revision" "$node_bin" -e '
const marker = {
  schemaVersion: 1,
  managedBy: process.env.IHUB_LAUNCHER_OWNER,
  launcherRevision: Number(process.env.IHUB_LAUNCHER_REVISION),
  sourceRoot: process.env.IHUB_SOURCE_ROOT,
  installedAt: new Date().toISOString(),
};
process.stdout.write(JSON.stringify(marker));
')"
  write_atomic_file "$launcher_marker_path" "$marker_content" 600
}

write_persistent_service_marker() {
  local marker_content
  marker_content="$(IHUB_SOURCE_ROOT="$repository_root" IHUB_SERVICE_OWNER="$persistent_owner" IHUB_WATCH_LABEL="$watch_label" IHUB_REFRESH_LABEL="$refresh_label" IHUB_WATCH_WRAPPER="$watch_wrapper_path" IHUB_REFRESH_WRAPPER="$refresh_wrapper_path" "$node_bin" -e '
const marker = {
  schemaVersion: 1,
  serviceRevision: 1,
  managedBy: process.env.IHUB_SERVICE_OWNER,
  sourceRoot: process.env.IHUB_SOURCE_ROOT,
  watchLabel: process.env.IHUB_WATCH_LABEL,
  refreshLabel: process.env.IHUB_REFRESH_LABEL,
  watchWrapperPath: process.env.IHUB_WATCH_WRAPPER,
  refreshWrapperPath: process.env.IHUB_REFRESH_WRAPPER,
  configuredAt: new Date().toISOString(),
};
process.stdout.write(JSON.stringify(marker));
')"
  write_atomic_file "$persistent_service_marker_path" "$marker_content" 600
}

assert_no_existing_persistent_service() {
  local service_marker_present=0
  if path_exists_any "$persistent_service_marker_path"; then
    persistent_service_marker_is_owned \
      || die "Refusing to change a foreign or unsafe persistent-service marker: $persistent_service_marker_path"
    service_marker_present=1
  fi
  if (( service_marker_present == 0 )) && { path_exists_any "$watch_wrapper_path" || path_exists_any "$refresh_wrapper_path"; }; then
    die 'Refusing to adopt persistent-service wrappers without the iHub service marker.'
  fi
  assert_wrapper_is_absent_or_owned "$watch_wrapper_path"
  assert_wrapper_is_absent_or_owned "$refresh_wrapper_path"

  local active_path disabled_path label
  for active_path in "$watch_plist_path" "$refresh_plist_path"; do
    if path_exists_any "$active_path"; then
      if [[ "$active_path" == "$watch_plist_path" ]]; then label="$watch_label"; else label="$refresh_label"; fi
      assert_owned_persistent_plist "$active_path" "$label"
      die "The iHub macOS persistent service is already configured at $active_path. Use --development-install-status or disable it cooperatively before changing it."
    fi
  done
  for disabled_path in "$watch_disabled_plist_path" "$refresh_disabled_plist_path"; do
    if path_exists_any "$disabled_path"; then
      if [[ "$disabled_path" == "$watch_disabled_plist_path" ]]; then label="$watch_label"; else label="$refresh_label"; fi
      assert_owned_persistent_plist "$disabled_path" "$label"
      if launch_agent_is_loaded "$label"; then
        die "A previously disabled iHub LaunchAgent is still loaded for this login session. Wait for its cooperative stop, then log out and back in before enabling it again; no running agent was forced out."
      fi
      if (( dry_run == 1 )); then
        printf '[dry-run] remove inactive, iHub-owned disabled plist: %s\n' "$disabled_path"
      else
        /bin/rm -f "$disabled_path" || die "Could not clear inactive iHub-owned disabled plist: $disabled_path"
      fi
    fi
  done
  for label in "$watch_label" "$refresh_label"; do
    if launch_agent_is_loaded "$label"; then
      die "A LaunchAgent with iHub's reserved label is already loaded without an owned active/disabled plist. It was left untouched; inspect it before enabling this service."
    fi
  done
}

write_service_status_block() {
  # This function's text is embedded in both generated wrappers. Status files
  # contain only lifecycle state, PID and time; no signing path or secret.
  # shellcheck disable=SC2016 # Emit literal Bash for the generated wrapper.
  printf '%s\n' \
    'write_service_status() {' \
    '  local state="$1"' \
    '  local exit_code="$2"' \
    '  local temporary="$status_path.$$.tmp"' \
    '  (' \
    '    umask 077' \
    '    {' \
    '      printf "schemaVersion=1\\n"' \
    '      printf "state=%s\\n" "$state"' \
    '      printf "pid=%s\\n" "$$"' \
    '      printf "exitCode=%s\\n" "$exit_code"' \
    '      printf "updatedAt=%s\\n" "$(/bin/date -u +%Y-%m-%dT%H:%M:%SZ)"' \
    '    } > "$temporary"' \
    '    /bin/mv "$temporary" "$status_path"' \
    '  )' \
    '}' \
    '' \
    'stop_requested() {' \
    '  if [[ -L "$stop_signal_path" ]]; then' \
    '    write_service_status "failed-unsafe-stop-signal" 1' \
    '    exit 1' \
    '  fi' \
    '  if [[ -e "$stop_signal_path" && ! -f "$stop_signal_path" ]]; then' \
    '    write_service_status "failed-unsafe-stop-signal" 1' \
    '    exit 1' \
    '  fi' \
    '  [[ -f "$stop_signal_path" ]]' \
    '}'
}

write_watch_wrapper() {
  local source_q key_q password_q path_q stop_q status_q
  source_q="$(shell_quote "$repository_root")"
  key_q="$(shell_quote "$configured_signing_key_path")"
  path_q="$(shell_quote "$service_path")"
  stop_q="$(shell_quote "$persistent_stop_signal_path")"
  status_q="$(shell_quote "$watch_status_path")"
  if [[ -n "$configured_signing_password_path" ]]; then password_q="$(shell_quote "$configured_signing_password_path")"; fi

  local content
  content="$(
    printf '%s\n' '#!/usr/bin/env bash' "# managedBy: $persistent_owner" 'set -euo pipefail'
    printf 'export PATH=%s\n' "$path_q"
    printf '%s\n' 'unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD'
    printf 'export IHUB_UPDATER_PRIVATE_KEY_PATH=%s\n' "$key_q"
    if [[ -n "$configured_signing_password_path" ]]; then printf 'export IHUB_UPDATER_PASSWORD_PATH=%s\n' "$password_q"; else printf '%s\n' 'unset IHUB_UPDATER_PASSWORD_PATH'; fi
    printf 'source_root=%s\n' "$source_q"
    printf 'stop_signal_path=%s\n' "$stop_q"
    printf 'status_path=%s\n\n' "$status_q"
    write_service_status_block
    # shellcheck disable=SC2016 # Emit literal Bash for the generated wrapper.
    printf '%s\n' '' \
      'if stop_requested; then' \
      '  write_service_status "stopped" 0' \
      '  exit 0' \
      'fi' \
      'development_script="$source_root/scripts/dev.sh"' \
      'if [[ ! -f "$development_script" || -L "$development_script" ]]; then' \
      '  write_service_status "failed-missing-development-script" 1' \
      '  exit 1' \
      'fi' \
      'write_service_status "watching" 0' \
      'set +e' \
      '/bin/bash "$development_script" --watch-install --watch-interval-seconds 5 --watch-stop-signal-path "$stop_signal_path"' \
      'exit_code=$?' \
      'set -e' \
      'if stop_requested; then' \
      '  write_service_status "stopped" "$exit_code"' \
      '  exit 0' \
      'fi' \
      'write_service_status "failed" "$exit_code"' \
      'exit "$exit_code"'
  )"
  write_atomic_file "$watch_wrapper_path" "$content" 700
}

write_refresh_wrapper() {
  local source_q key_q password_q path_q stop_q status_q
  source_q="$(shell_quote "$repository_root")"
  key_q="$(shell_quote "$configured_signing_key_path")"
  path_q="$(shell_quote "$service_path")"
  stop_q="$(shell_quote "$persistent_stop_signal_path")"
  status_q="$(shell_quote "$refresh_status_path")"
  if [[ -n "$configured_signing_password_path" ]]; then password_q="$(shell_quote "$configured_signing_password_path")"; fi

  local content
  content="$(
    printf '%s\n' '#!/usr/bin/env bash' "# managedBy: $persistent_owner" 'set -euo pipefail'
    printf 'export PATH=%s\n' "$path_q"
    printf '%s\n' 'unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD'
    printf 'export IHUB_UPDATER_PRIVATE_KEY_PATH=%s\n' "$key_q"
    if [[ -n "$configured_signing_password_path" ]]; then printf 'export IHUB_UPDATER_PASSWORD_PATH=%s\n' "$password_q"; else printf '%s\n' 'unset IHUB_UPDATER_PASSWORD_PATH'; fi
    printf 'source_root=%s\n' "$source_q"
    printf 'stop_signal_path=%s\n' "$stop_q"
    printf 'status_path=%s\n\n' "$status_q"
    write_service_status_block
    # shellcheck disable=SC2016 # Emit literal Bash for the generated wrapper.
    printf '%s\n' '' \
      'if stop_requested; then' \
      '  write_service_status "stopped" 0' \
      '  exit 0' \
      'fi' \
      'development_script="$source_root/scripts/dev.sh"' \
      'if [[ ! -f "$development_script" || -L "$development_script" ]]; then' \
      '  write_service_status "failed-missing-development-script" 1' \
      '  exit 1' \
      'fi' \
      'write_service_status "refreshing" 0' \
      'set +e' \
      '/bin/bash "$development_script" --update-if-clean --verify-only --skip-install --skip-check' \
      'exit_code=$?' \
      'set -e' \
      'if stop_requested; then' \
      '  write_service_status "stopped" "$exit_code"' \
      '  exit 0' \
      'fi' \
      'if [[ "$exit_code" == "0" ]]; then' \
      '  write_service_status "completed-or-safely-skipped" 0' \
      '  exit 0' \
      'fi' \
      'write_service_status "failed" "$exit_code"' \
      'exit "$exit_code"'
  )"
  write_atomic_file "$refresh_wrapper_path" "$content" 700
}

write_launch_agent_plist() {
  local destination="$1"
  local label="$2"
  local wrapper_path="$3"
  local stdout_path="$4"
  local stderr_path="$5"
  local start_interval_seconds="$6"
  local label_xml wrapper_xml stdout_xml stderr_xml
  label_xml="$(xml_escape "$label")"
  wrapper_xml="$(xml_escape "$wrapper_path")"
  stdout_xml="$(xml_escape "$stdout_path")"
  stderr_xml="$(xml_escape "$stderr_path")"

  local interval_block=''
  if [[ "$start_interval_seconds" != '0' ]]; then
    interval_block="$(printf '    <key>StartInterval</key>\n    <integer>%s</integer>\n' "$start_interval_seconds")"
    interval_block+=$'\n'
  fi

  local content
  content="<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
  <dict>
    <key>Label</key>
    <string>$label_xml</string>
    <key>ProgramArguments</key>
    <array>
      <string>/bin/bash</string>
      <string>$wrapper_xml</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>ThrottleInterval</key>
    <integer>30</integer>
$interval_block    <key>StandardOutPath</key>
    <string>$stdout_xml</string>
    <key>StandardErrorPath</key>
    <string>$stderr_xml</string>
  </dict>
</plist>
"
  write_atomic_plist "$destination" "$content"
}

write_persistent_stop_signal() {
  local content
  content="schemaVersion=1
requestedAt=$(/bin/date -u +%Y-%m-%dT%H:%M:%SZ)
"
  write_atomic_file "$persistent_stop_signal_path" "$content" 600
}

remove_persistent_stop_signal() {
  if ! path_exists_any "$persistent_stop_signal_path"; then
    return 0
  fi
  [[ -f "$persistent_stop_signal_path" && ! -L "$persistent_stop_signal_path" ]] \
    || die "Refusing to remove an unsafe persistent-service stop signal: $persistent_stop_signal_path"
  /bin/rm -f "$persistent_stop_signal_path" || die "Could not remove persistent-service stop signal: $persistent_stop_signal_path"
}

rename_active_plist_to_disabled() {
  local active_path="$1"
  local disabled_path="$2"
  local label="$3"
  if ! path_exists_any "$active_path"; then
    return 0
  fi
  assert_owned_persistent_plist "$active_path" "$label"
  if path_exists_any "$disabled_path"; then
    assert_owned_persistent_plist "$disabled_path" "$label"
    die "Both active and disabled iHub persistent-service plists exist; neither was changed: $active_path"
  fi
  /bin/mv "$active_path" "$disabled_path" || die "Could not move iHub-owned LaunchAgent out of future-login discovery: $active_path"
}

print_status_file() {
  local description="$1"
  local status_path="$2"
  printf '%s: ' "$description"
  if ! path_exists_any "$status_path"; then
    printf 'no status file\n'
    return 0
  fi
  if [[ ! -f "$status_path" || -L "$status_path" ]]; then
    printf 'unsafe status path (%s)\n' "$status_path"
    return 0
  fi
  /usr/bin/sed -n '1,5p' "$status_path" | /usr/bin/tr '\n' ';'
  printf '\n'
}

report_launch_agent_status() {
  local description="$1"
  local plist_path="$2"
  local disabled_path="$3"
  local label="$4"
  printf '%s: ' "$description"
  if path_exists_any "$plist_path"; then
    if persistent_plist_is_owned "$plist_path" "$label"; then
      if launch_agent_is_loaded "$label"; then printf 'configured, loaded\n'; else printf 'configured, not currently loaded\n'; fi
    else
      printf 'foreign or unsafe plist at %s\n' "$plist_path"
    fi
    return 0
  fi
  if path_exists_any "$disabled_path"; then
    if persistent_plist_is_owned "$disabled_path" "$label"; then
      if launch_agent_is_loaded "$label"; then printf 'disable pending; loaded until cooperative exit/logout\n'; else printf 'disabled for future logins\n'; fi
    else
      printf 'foreign or unsafe disabled plist at %s\n' "$disabled_path"
    fi
    return 0
  fi
  if launch_agent_is_loaded "$label"; then printf 'loaded without an owned plist; left untouched\n'; else printf 'not configured\n'; fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-launcher|--enable-persistent-development-install|--disable-persistent-development-install|--development-install-status)
      [[ -z "$mode" ]] || die 'Choose exactly one mode.'
      mode="$1"
      shift
      ;;
    --signing-key-path)
      [[ $# -ge 2 ]] || die '--signing-key-path requires an absolute file path.'
      signing_key_path="$2"
      shift 2
      ;;
    --signing-password-path)
      [[ $# -ge 2 ]] || die '--signing-password-path requires an absolute file path.'
      signing_password_path="$2"
      shift 2
      ;;
    --upstream-check-minutes)
      [[ $# -ge 2 ]] || die '--upstream-check-minutes requires an integer from 10 to 240.'
      upstream_check_minutes="$2"
      shift 2
      ;;
    --dry-run)
      dry_run=1
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

[[ -n "$mode" ]] || {
  usage >&2
  exit 2
}
if ! [[ "$upstream_check_minutes" =~ ^[0-9]+$ ]] || (( upstream_check_minutes < 10 || upstream_check_minutes > 240 )); then
  die '--upstream-check-minutes must be an integer from 10 to 240.'
fi
[[ "$(/usr/bin/uname -s)" == 'Darwin' ]] || die 'scripts/install-dev.sh is for macOS only.'

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repository_root="$(cd -- "$script_dir/.." && pwd -P)"
developer_script="$repository_root/scripts/dev.sh"
[[ -f "$developer_script" && ! -L "$developer_script" ]] || die "This does not look like an iHub checkout: missing $developer_script"
for required_file in package.json pnpm-lock.yaml src-tauri/tauri.conf.json; do
  [[ -f "$repository_root/$required_file" ]] || die "This does not look like an iHub checkout: missing $required_file"
done

node_bin="$(absolute_command_path node)"
[[ -n "${HOME:-}" && -d "$HOME" ]] || die 'HOME is unavailable; cannot configure a user-local macOS developer launcher.'
canonical_home="$(cd -- "$HOME" && pwd -P)"
assert_no_line_breaks "$canonical_home" 'HOME'
support_root="$canonical_home/Library/Application Support/iHub Development"
launcher_marker_path="$support_root/launcher.json"
persistent_service_marker_path="$support_root/persistent-development-service.json"
persistent_stop_signal_path="$support_root/persistent-development-install.stop"
watch_wrapper_path="$support_root/Run iHub Development Watch Service.sh"
refresh_wrapper_path="$support_root/Run iHub Development Safe Refresh.sh"
watch_status_path="$support_root/persistent-development-watch-status.txt"
refresh_status_path="$support_root/persistent-development-refresh-status.txt"
log_root="$support_root/logs"
launch_agents_root="$canonical_home/Library/LaunchAgents"
watch_plist_path="$launch_agents_root/$watch_label.plist"
refresh_plist_path="$launch_agents_root/$refresh_label.plist"
watch_disabled_plist_path="$watch_plist_path.disabled"
refresh_disabled_plist_path="$refresh_plist_path.disabled"
current_user_id="$(/usr/bin/id -u)"

case "$mode" in
  --install-launcher)
    ensure_support_root_for_launcher
    if [[ -f "$launcher_marker_path" ]]; then
      assert_owned_launcher_marker
      if [[ "$launcher_marker_source_root" != "$repository_root" ]] && { path_exists_any "$watch_plist_path" || path_exists_any "$refresh_plist_path" || path_exists_any "$watch_disabled_plist_path" || path_exists_any "$refresh_disabled_plist_path"; }; then
        die 'The existing launcher points to another worktree with persistent-service artifacts. Disable that service cooperatively before retargeting the launcher.'
      fi
    fi
    if (( dry_run == 1 )); then
      printf '[dry-run] write iHub macOS development launcher marker for: %s\n' "$repository_root"
    else
      write_launcher_marker
      printf 'iHub macOS development launcher installed. No LaunchAgent, app process, or source update was started.\n'
      printf '  Source worktree: %s\n' "$repository_root"
      printf '  Marker:          %s\n' "$launcher_marker_path"
    fi
    ;;

  --enable-persistent-development-install)
    [[ -n "$signing_key_path" ]] || die '--enable-persistent-development-install requires --signing-key-path. The key contents are never copied into a plist or log.'
    [[ -z "$signing_password_path" || -n "$signing_key_path" ]] || die '--signing-password-path is valid only with --signing-key-path.'
    for required_command in git corepack cargo /bin/bash /bin/launchctl /usr/bin/plutil /usr/libexec/PlistBuddy; do
      if [[ "$required_command" == /* ]]; then [[ -x "$required_command" ]] || die "Required macOS executable is unavailable: $required_command"; else absolute_command_path "$required_command" >/dev/null; fi
    done
    assert_launcher_targets_current_worktree
    configured_signing_key_path="$(canonicalize_existing_regular_file "$signing_key_path" 'Updater signing key')"
    configured_signing_password_path=''
    if [[ -n "$signing_password_path" ]]; then
      configured_signing_password_path="$(canonicalize_existing_regular_file "$signing_password_path" 'Updater signing password file')"
    fi
    git_bin="$(absolute_command_path git)"
    corepack_bin="$(absolute_command_path corepack)"
    cargo_bin="$(absolute_command_path cargo)"
    for tool_path in "$node_bin" "$git_bin" "$corepack_bin" "$cargo_bin"; do
      [[ "$tool_path" != *:* ]] || die "A persistent-service tool path cannot contain ':': $tool_path"
      assert_no_line_breaks "$tool_path" 'Persistent service tool path'
    done
    service_path="$(dirname -- "$node_bin"):$(dirname -- "$git_bin"):$(dirname -- "$corepack_bin"):$(dirname -- "$cargo_bin"):/usr/bin:/bin:/usr/sbin:/sbin"
    assert_no_line_breaks "$service_path" 'Persistent service PATH'
    ensure_support_root_for_launcher
    ensure_launch_agents_root
    assert_no_existing_persistent_service
    if (( dry_run == 1 )); then
      printf '[dry-run] create protected log directory: %s\n' "$log_root"
      printf '[dry-run] write exact Bash wrappers below: %s\n' "$support_root"
      printf '[dry-run] write iHub-owned LaunchAgents: %s and %s\n' "$watch_plist_path" "$refresh_plist_path"
      printf '[dry-run] bootstrap current-user labels: gui/%s/%s and gui/%s/%s\n' "$current_user_id" "$watch_label" "$current_user_id" "$refresh_label"
      exit 0
    fi
    /bin/mkdir -p "$log_root" || die "Could not create persistent-service log directory: $log_root"
    [[ -d "$log_root" && ! -L "$log_root" ]] || die "Persistent-service log directory is unsafe: $log_root"
    /bin/chmod 700 "$log_root" || die "Could not protect persistent-service log directory: $log_root"
    write_persistent_service_marker
    write_watch_wrapper
    write_refresh_wrapper
    remove_persistent_stop_signal
    refresh_seconds=$(( upstream_check_minutes * 60 ))
    write_launch_agent_plist "$watch_plist_path" "$watch_label" "$watch_wrapper_path" "$log_root/watch.stdout.log" "$log_root/watch.stderr.log" 0
    write_launch_agent_plist "$refresh_plist_path" "$refresh_label" "$refresh_wrapper_path" "$log_root/refresh.stdout.log" "$log_root/refresh.stderr.log" "$refresh_seconds"
    if ! /bin/launchctl bootstrap "gui/$current_user_id" "$watch_plist_path"; then
      write_persistent_stop_signal
      die "Could not bootstrap the iHub watch LaunchAgent. Its stop signal was written; no agent was forced out."
    fi
    if ! /bin/launchctl bootstrap "gui/$current_user_id" "$refresh_plist_path"; then
      write_persistent_stop_signal
      die "The watcher may be loaded, but the refresh LaunchAgent could not be bootstrapped. A cooperative stop signal was written; no running agent was forced out."
    fi
    printf 'Enabled the current-user macOS iHub persistent development service. It does not launch or stop iHub.\n'
    printf '  Watch agent:   %s\n' "$watch_label"
    printf '  Refresh agent: %s (every %s minutes)\n' "$refresh_label" "$upstream_check_minutes"
    printf '  Status:        bash scripts/install-dev.sh --development-install-status\n'
    ;;

  --disable-persistent-development-install)
    if [[ ! -d "$support_root" || -L "$support_root" || ! -f "$launcher_marker_path" || -L "$launcher_marker_path" ]]; then
      printf 'No trusted iHub macOS development launcher is installed; no persistent service was changed.\n'
      exit 0
    fi
    assert_owned_launcher_marker
    if path_exists_any "$persistent_service_marker_path"; then
      persistent_service_marker_is_owned \
        || die "Refusing to change a foreign or unsafe persistent-service marker: $persistent_service_marker_path"
    fi
    assert_plist_is_absent_or_owned "$watch_plist_path" "$watch_label"
    assert_plist_is_absent_or_owned "$refresh_plist_path" "$refresh_label"
    assert_plist_is_absent_or_owned "$watch_disabled_plist_path" "$watch_label"
    assert_plist_is_absent_or_owned "$refresh_disabled_plist_path" "$refresh_label"
    if (( dry_run == 1 )); then
      printf '[dry-run] write cooperative stop signal: %s\n' "$persistent_stop_signal_path"
      printf '[dry-run] move only iHub-owned active plists out of future-login discovery. No launchctl bootout will be called.\n'
      exit 0
    fi
    write_persistent_stop_signal
    rename_active_plist_to_disabled "$watch_plist_path" "$watch_disabled_plist_path" "$watch_label"
    rename_active_plist_to_disabled "$refresh_plist_path" "$refresh_disabled_plist_path" "$refresh_label"
    printf 'Requested cooperative shutdown and disabled iHub LaunchAgents for future logins.\n'
    printf 'No launchctl bootout, forced process termination, or iHub action was performed. A currently loaded wrapper observes the stop signal at its next safe boundary; log out and back in before enabling it again.\n'
    ;;

  --development-install-status)
    printf 'iHub macOS development persistent-install status\n'
    printf 'Support root: %s\n' "$support_root"
    if [[ -f "$launcher_marker_path" && ! -L "$launcher_marker_path" ]]; then
      if launcher_marker_is_owned; then
        printf 'Launcher: trusted (source: %s)\n' "$launcher_marker_source_root"
      else
        printf 'Launcher: invalid or foreign marker\n'
      fi
    else
      printf 'Launcher: not installed\n'
    fi
    if [[ -f "$persistent_service_marker_path" && ! -L "$persistent_service_marker_path" ]]; then
      if persistent_service_marker_is_owned; then
        printf 'Persistent-service marker: trusted (source: %s)\n' "$persistent_service_marker_source_root"
      else
        printf 'Persistent-service marker: invalid or foreign\n'
      fi
    elif path_exists_any "$persistent_service_marker_path"; then
      printf 'Persistent-service marker: unsafe path\n'
    else
      printf 'Persistent-service marker: not installed\n'
    fi
    report_launch_agent_status 'Watch LaunchAgent' "$watch_plist_path" "$watch_disabled_plist_path" "$watch_label"
    report_launch_agent_status 'Refresh LaunchAgent' "$refresh_plist_path" "$refresh_disabled_plist_path" "$refresh_label"
    if [[ -f "$persistent_stop_signal_path" && ! -L "$persistent_stop_signal_path" ]]; then printf 'Stop signal: present (cooperative shutdown requested)\n'; elif path_exists_any "$persistent_stop_signal_path"; then printf 'Stop signal: unsafe path\n'; else printf 'Stop signal: absent\n'; fi
    print_status_file 'Watch wrapper health' "$watch_status_path"
    print_status_file 'Refresh wrapper health' "$refresh_status_path"
    ;;
esac
