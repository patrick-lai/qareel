#!/bin/sh
set -eu

releases='https://github.com/patrick-lai/qareel/releases'
install_root="${QAREEL_INSTALL_DIR:-$HOME/.qareel}"
bin_dir="${QAREEL_BIN_DIR:-$HOME/.local/bin}"

fail() {
    printf 'qareel install: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 is required; install it and run this again."
}

need uname
need curl
need tar
need mktemp
need awk

case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "qareel runs on macOS and Linux; this is $(uname -s)." ;;
esac
case "$(uname -m)" in
    arm64|aarch64) arch=arm64 ;;
    x86_64|amd64) arch=x64 ;;
    *) fail "qareel runs on arm64 and x64; this is $(uname -m)." ;;
esac
if [ "$os" = darwin ] && [ "$arch" = x64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
    arch=arm64
fi
platform="$os-$arch"

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
    fail 'sha256sum or shasum is required to verify the download.'
fi

link="$bin_dir/qareel"
if [ -e "$link" ] && [ ! -L "$link" ]; then
    fail "$link exists and is not a link from a previous install; move it away or set QAREEL_BIN_DIR."
fi

requested="${QAREEL_VERSION:-}"
requested="${requested#v}"
if [ -n "$requested" ]; then
    case "$requested" in
        *[!0-9A-Za-z.+-]*|.*) fail "QAREEL_VERSION is not a valid version: $requested" ;;
    esac
    base="$releases/download/v$requested"
else
    base="$releases/latest/download"
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/qareel-install.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 1' INT TERM

download() {
    curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
        --retry 3 --connect-timeout 20 --max-time "$2" --output "$3" "$base/$1" \
        || fail "could not download $base/$1"
}

download SHA256SUMS 60 "$work/SHA256SUMS"
line=$(awk -v suffix="-$platform.tar.gz" '
    length($2) > length(suffix) && substr($2, 1, 7) == "qareel-" && substr($2, length($2) - length(suffix) + 1) == suffix { print; count++ }
    END { if (count != 1) exit 1 }' "$work/SHA256SUMS") || fail "the release has no single qareel build for $platform."
expected=${line%% *}
archive=${line##* }
version=${archive#qareel-}
version=${version%"-$platform.tar.gz"}
case "$version" in
    ''|*[!0-9A-Za-z.+-]*|.*) fail "the release lists an unexpected file name: $archive" ;;
esac
if [ -n "$requested" ] && [ "$version" != "$requested" ]; then
    fail "the v$requested release lists $archive instead of qareel-$requested-$platform.tar.gz."
fi
case "$expected" in
    *[!0-9a-f]*) fail 'SHA256SUMS is malformed.' ;;
esac
[ "${#expected}" -eq 64 ] || fail 'SHA256SUMS is malformed.'

printf 'Downloading qareel %s for %s\n' "$version" "$platform"
download "$archive" 900 "$work/$archive"
actual=$(sha256 "$work/$archive")
[ "$actual" = "$expected" ] || fail "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed."

mkdir -p "$install_root"
install_root=$(CDPATH='' cd -- "$install_root" && pwd)
staging=$(mktemp -d "$install_root/.install.XXXXXX")
trap 'rm -rf "$work" "$staging"' EXIT
tar -xzf "$work/$archive" -C "$staging"
[ -x "$staging/qareel/bin/qareel" ] || fail "$archive has no qareel/bin/qareel; nothing was installed."
destination="$install_root/$version"
rm -rf "$destination"
mv "$staging/qareel" "$destination"

mkdir -p "$bin_dir"
ln -s "$destination/bin/qareel" "$bin_dir/.qareel.$$"
mv -f "$bin_dir/.qareel.$$" "$link"

printf 'Installed qareel %s to %s\n' "$version" "$destination"
case ":${PATH:-}:" in
    *":$bin_dir:"*) ;;
    *) printf '%s is not on your PATH. Add it with:\n  export PATH="%s:$%s"\n' "$bin_dir" "$bin_dir" PATH ;;
esac
printf 'Next: qareel guide\n'
