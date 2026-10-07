#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

usage() {
    cat <<'EOF'
Usage: scripts/build-dist.sh [--target TRIPLE] [--zig] [--build-only | --package-only]

Builds the release CLI (and the Swift host on macOS), assembles the install
tree in dist/stage/<platform>/qareel/ and packs it as
dist/qareel-<version>-<platform>.tar.gz with a .sha256 file.

  --target TRIPLE  Rust target; defaults to the host.
  --zig            Build the CLI with cargo zigbuild (Linux musl cross builds).
  --build-only     Build and assemble the tree without packing it (sign it next).
  --package-only   Pack a tree that is already assembled.
EOF
}

fail() {
    printf 'build-dist: %s\n' "$1" >&2
    exit 1
}

target=''
zig=0
build=1
package=1
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            [ "$#" -ge 2 ] || fail '--target needs a Rust target triple.'
            target=$2
            shift 2
            ;;
        --zig) zig=1; shift ;;
        --build-only) package=0; shift ;;
        --package-only) build=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; fail "unknown option: $1" ;;
    esac
done
[ "$build" -eq 1 ] || [ "$package" -eq 1 ] || fail '--build-only and --package-only cannot be combined.'

if [ -n "$target" ]; then
    case "$target" in
        aarch64-apple-darwin) os=darwin; arch=arm64 ;;
        x86_64-apple-darwin) os=darwin; arch=x64 ;;
        aarch64-unknown-linux-*) os=linux; arch=arm64 ;;
        x86_64-unknown-linux-*) os=linux; arch=x64 ;;
        *) fail "unsupported target: $target" ;;
    esac
    cli="$root/target/$target/release/qareel"
else
    case "$(uname -s)" in
        Darwin) os=darwin ;;
        Linux) os=linux ;;
        *) fail "unsupported operating system: $(uname -s)" ;;
    esac
    case "$(uname -m)" in
        arm64|aarch64) arch=arm64 ;;
        x86_64|amd64) arch=x64 ;;
        *) fail "unsupported architecture: $(uname -m)" ;;
    esac
    cli="$root/target/release/qareel"
fi
platform="$os-$arch"
case "$arch" in
    arm64) swift_arch=arm64 ;;
    x64) swift_arch=x86_64 ;;
esac
dist="$root/dist"
tree="$dist/stage/$platform/qareel"

if [ "$build" -eq 1 ]; then
    if [ "$os" = darwin ] && [ -z "${DEVELOPER_DIR:-}" ]; then
        case "$(xcode-select -p 2>/dev/null || true)" in
            *CommandLineTools*)
                if [ -d /Applications/Xcode.app/Contents/Developer ]; then
                    DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
                    export DEVELOPER_DIR
                fi
                ;;
        esac
    fi

    set -- --release --locked -p qareel --manifest-path "$root/Cargo.toml"
    if [ -n "$target" ]; then
        set -- "$@" --target "$target"
    fi
    if [ "$zig" -eq 1 ]; then
        cargo zigbuild "$@"
    else
        cargo build "$@"
    fi
    [ -x "$cli" ] || fail "cargo did not produce $cli."

    if [ "$os" = darwin ]; then
        set -- -c release --package-path "$root/host-macos" --arch "$swift_arch"
        swift build "$@"
        products="$(swift build "$@" --show-bin-path)"
        host="$products/qareel-host"
        engine="$products/libQareelEngine.dylib"
        [ -x "$host" ] || fail "swift build did not produce $host."
        [ -f "$engine" ] || fail "swift build did not produce $engine."
        lipo "$host" -verify_arch "$swift_arch" || fail "$host is not built for $swift_arch."
        lipo "$engine" -verify_arch "$swift_arch" || fail "$engine is not built for $swift_arch."
    fi

    for file in reel.py reel_sound.py reel_assets; do
        [ -e "$root/reel/$file" ] || fail "missing reel/$file."
    done

    rm -rf "$dist/stage/$platform"
    mkdir -p "$tree/bin" "$tree/share/qareel/reel"
    cp "$cli" "$tree/bin/qareel"
    if [ "$os" = darwin ]; then
        mkdir -p "$tree/libexec" "$tree/lib"
        cp "$host" "$tree/libexec/qareel-host"
        cp "$engine" "$tree/lib/libQareelEngine.dylib"
    fi
    cp "$root/reel/reel.py" "$root/reel/reel_sound.py" "$tree/share/qareel/reel/"
    cp -R "$root/reel/reel_assets" "$tree/share/qareel/reel/reel_assets"
    cp "$root/LICENSE" "$tree/LICENSE"
    find "$tree" \( -name __pycache__ -o -name '*.pyc' -o -name .DS_Store \) -prune -exec rm -rf {} +
    find "$tree" -type d -exec chmod 0755 {} +
    find "$tree" -type f -exec chmod 0644 {} +
    chmod 0755 "$tree/bin/qareel"
    if [ "$os" = darwin ]; then
        chmod 0755 "$tree/libexec/qareel-host" "$tree/lib/libQareelEngine.dylib"
    fi
    printf 'Assembled %s\n' "$tree"
fi

[ "$package" -eq 1 ] || exit 0

[ -x "$tree/bin/qareel" ] || fail "missing $tree/bin/qareel; run without --package-only first."
if [ "$os" = darwin ]; then
    [ -x "$tree/libexec/qareel-host" ] || fail "missing $tree/libexec/qareel-host; run without --package-only first."
    [ -f "$tree/lib/libQareelEngine.dylib" ] || fail "missing $tree/lib/libQareelEngine.dylib; run without --package-only first."
fi

pkgid=$(cargo pkgid -p qareel --manifest-path "$root/Cargo.toml")
version=${pkgid##*[@#]}
case "$version" in
    ''|*[!0-9A-Za-z.+-]*) fail "could not read the qareel version from Cargo (got '$pkgid')." ;;
esac

name="qareel-$version-$platform.tar.gz"
scratch=$(mktemp -d "$dist/.pack.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 1' INT TERM
if [ "$(uname -s)" = Darwin ]; then
    COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs -czf "$scratch/$name" -C "$dist/stage/$platform" qareel
else
    tar -czf "$scratch/$name" -C "$dist/stage/$platform" qareel
fi
if command -v sha256sum >/dev/null 2>&1; then
    sum=$(cd "$scratch" && sha256sum "$name")
else
    sum=$(cd "$scratch" && shasum -a 256 "$name")
fi
printf '%s\n' "$sum" > "$scratch/$name.sha256"
mv -f "$scratch/$name" "$dist/$name"
mv -f "$scratch/$name.sha256" "$dist/$name.sha256"
printf '%s\n' "$dist/$name"
