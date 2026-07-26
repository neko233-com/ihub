#!/usr/bin/env bash
# iHub bootstrap installer for macOS. The DMG is verified against the
# SHA256SUMS.txt asset from the selected GitHub Release before installation.

set -euo pipefail

repository="${IHUB_REPOSITORY:-neko233-com/ihub}"
version="${IHUB_VERSION:-latest}"
application_dir="${IHUB_APPLICATION_DIR:-/Applications}"
launch_after_install=1
require_signature=0
temp_dir=""
mount_point=""
mounted=0

usage() {
  printf '%s\n' \
    'Usage: install.sh [options]' \
    '' \
    '  --repository OWNER/REPO   GitHub repository (default: neko233-com/ihub)' \
    '  --version TAG             Release tag or latest (default: latest)' \
    '  --application-dir PATH    /Applications or ~/Applications' \
    '  --no-launch               Do not launch iHub after installing' \
    '  --require-signature       Require a valid Apple code signature and Gatekeeper assessment' \
    '  -h, --help                Show this help'
}

die() {
  printf 'iHub installer error: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [[ "${mounted:-0}" == '1' && -n "${mount_point:-}" ]]; then
    /usr/bin/hdiutil detach "$mount_point" -quiet >/dev/null 2>&1 || true
  fi
  if [[ -n "${temp_dir:-}" && -d "$temp_dir" ]]; then
    rm -rf -- "$temp_dir"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repository)
      [[ $# -ge 2 ]] || die '--repository requires OWNER/REPO.'
      repository="$2"
      shift 2
      ;;
    --version)
      [[ $# -ge 2 ]] || die '--version requires a tag or latest.'
      version="$2"
      shift 2
      ;;
    --application-dir)
      [[ $# -ge 2 ]] || die '--application-dir requires a path.'
      application_dir="$2"
      shift 2
      ;;
    --no-launch)
      launch_after_install=0
      shift
      ;;
    --require-signature)
      require_signature=1
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

[[ "$(/usr/bin/uname -s)" == 'Darwin' ]] || die 'scripts/install.sh installs iHub on macOS only.'
[[ "$repository" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die 'Repository must be in owner/repository form.'
[[ "$version" == 'latest' || "$version" =~ ^v?[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || die 'Version must be latest or a simple release tag such as v0.1.0.'

case "$application_dir" in
  /Applications|"$HOME/Applications")
    ;;
  *)
    die 'For safety, --application-dir must be /Applications or ~/Applications.'
    ;;
esac

for required_command in curl shasum osascript hdiutil ditto codesign; do
  command -v "$required_command" >/dev/null 2>&1 || die "Required macOS command is unavailable: $required_command"
done

machine_arch="$(/usr/bin/uname -m)"
if [[ "$machine_arch" == 'x86_64' && "$(/usr/sbin/sysctl -in sysctl.proc_translated 2>/dev/null || true)" == '1' ]]; then
  # A shell launched under Rosetta is still running on Apple Silicon.
  release_arch='aarch64'
elif [[ "$machine_arch" == 'arm64' ]]; then
  release_arch='aarch64'
elif [[ "$machine_arch" == 'x86_64' ]]; then
  release_arch='x64'
else
  die "Unsupported macOS architecture: $machine_arch"
fi

temp_dir="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/ihub-install.XXXXXX")"
mount_point="$temp_dir/mount"
trap cleanup EXIT INT TERM

download() {
  local url="$1"
  local destination="$2"
  [[ "$url" == https://* ]] || die "Refusing a non-HTTPS download URL: $url"

  /usr/bin/curl \
    --fail \
    --location \
    --proto '=https' \
    --tlsv1.2 \
    --retry 3 \
    --connect-timeout 15 \
    --silent \
    --show-error \
    -H 'Accept: application/vnd.github+json' \
    "$url" \
    --output "$destination"
}

api_root="https://api.github.com/repos/${repository}/releases"
if [[ "$version" == 'latest' ]]; then
  release_url="$api_root/latest"
else
  release_url="$api_root/tags/$version"
fi

release_json="$temp_dir/release.json"
download "$release_url" "$release_json"

asset_metadata="$(
  RELEASE_FILE="$release_json" IHUB_ARCH="$release_arch" /usr/bin/osascript -l JavaScript -e '
    ObjC.import("Foundation");
    const environment = $.NSProcessInfo.processInfo.environment;
    const releasePath = ObjC.unwrap(environment.objectForKey("RELEASE_FILE"));
    const arch = ObjC.unwrap(environment.objectForKey("IHUB_ARCH"));
    const data = $.NSData.dataWithContentsOfFile(releasePath);
    if (!data) throw new Error("Unable to read release metadata");
    const text = ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding));
    const release = JSON.parse(text);
    if (release.draft === true) throw new Error("Refusing a draft GitHub Release");
    const dmgPattern = new RegExp("^ihub_[^_]+_darwin_" + arch + "\\.dmg$", "i");
    const dmg = release.assets.find((asset) => dmgPattern.test(asset.name));
    const checksums = release.assets.find((asset) => asset.name === "SHA256SUMS.txt");
    if (!dmg || !checksums) throw new Error("The release is missing the required macOS DMG or SHA256SUMS.txt asset");
    console.log(dmg.name + "\t" + dmg.browser_download_url + "\t" + checksums.browser_download_url);
  '
)" || die 'Could not select a verified iHub macOS asset from the GitHub Release.'

IFS=$'\t' read -r dmg_name dmg_url checksums_url <<< "$asset_metadata"
[[ -n "$dmg_name" && -n "$dmg_url" && -n "$checksums_url" ]] || die 'Release metadata was incomplete.'
[[ "$dmg_name" != *'/'* && "$dmg_name" != *$'\\'* ]] || die 'Refusing an unsafe DMG asset name.'

dmg_path="$temp_dir/$dmg_name"
checksums_path="$temp_dir/SHA256SUMS.txt"
printf 'Downloading iHub %s for macOS %s...\n' "$version" "$release_arch"
download "$checksums_url" "$checksums_path"
download "$dmg_url" "$dmg_path"

expected_hash="$(/usr/bin/awk -v asset="$dmg_name" '$2 == "*" asset || $2 == asset { print tolower($1); exit }' "$checksums_path")"
[[ "$expected_hash" =~ ^[0-9a-f]{64}$ ]] || die "SHA256SUMS.txt does not contain a checksum for $dmg_name."
actual_hash="$(/usr/bin/shasum -a 256 "$dmg_path" | /usr/bin/awk '{ print tolower($1) }')"
[[ "$actual_hash" == "$expected_hash" ]] || die "SHA-256 verification failed for $dmg_name."
printf '%s\n' 'SHA-256 verification passed.'

/bin/mkdir -p "$mount_point"
/usr/bin/hdiutil attach -nobrowse -readonly -mountpoint "$mount_point" "$dmg_path" >/dev/null
mounted=1
app_path="$(/usr/bin/find "$mount_point" -type d -name '*.app' -prune -print | /usr/bin/head -n 1)"
[[ -n "$app_path" && -d "$app_path" ]] || die 'The verified DMG did not contain an application bundle.'

if /usr/bin/codesign --verify --deep --strict --verbose=2 "$app_path" >/dev/null 2>&1; then
  printf '%s\n' 'Apple code-signature verification passed.'
elif [[ "$require_signature" == '1' ]]; then
  die 'Apple code-signature verification failed.'
else
  printf '%s\n' 'Warning: Apple code-signature verification did not pass. The SHA-256 manifest is valid; use --require-signature for a signed-only policy.' >&2
fi

if [[ "$require_signature" == '1' ]] && ! /usr/sbin/spctl --assess --type execute --verbose=2 "$app_path" >/dev/null 2>&1; then
  die 'Gatekeeper assessment failed.'
fi

run_privileged() {
  if [[ -w "$application_dir" || (! -e "$application_dir" && -w "$(/usr/bin/dirname "$application_dir")") ]]; then
    "$@"
  else
    /usr/bin/sudo "$@"
  fi
}

run_privileged /bin/mkdir -p "$application_dir"
destination="$application_dir/iHub.app"
staging="$application_dir/.iHub.installing.$$"
previous="$application_dir/.iHub.previous.$$"

# Copy to a sibling staging directory first. Existing iHub.app is replaced only
# after the full, checksum-verified application bundle has been copied.
run_privileged /bin/rm -rf -- "$staging"
run_privileged /usr/bin/ditto "$app_path" "$staging"
if [[ -e "$destination" ]]; then
  run_privileged /bin/rm -rf -- "$previous"
  run_privileged /bin/mv "$destination" "$previous"
fi
if ! run_privileged /bin/mv "$staging" "$destination"; then
  if [[ -e "$previous" ]]; then
    run_privileged /bin/mv "$previous" "$destination" || true
  fi
  die 'Could not move the new iHub application into place.'
fi
if [[ -e "$previous" ]]; then
  run_privileged /bin/rm -rf -- "$previous"
fi

/usr/bin/hdiutil detach "$mount_point" -quiet >/dev/null
mounted=0
printf 'iHub installed to %s\n' "$destination"

if [[ "$launch_after_install" == '1' ]]; then
  /usr/bin/open "$destination"
fi
