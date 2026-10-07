#!/bin/sh
set -eu

usage() {
    cat <<'EOF'
Usage: scripts/macos-sign.sh BINARY...

Signs each binary with the Developer ID identity (hardened runtime, secure
timestamp) from a temporary keychain, then notarizes them as one zip.

Reads APPLE_DEVELOPER_ID_P12_BASE64, APPLE_DEVELOPER_ID_P12_PASSWORD,
APPLE_SIGNING_IDENTITY, APPLE_NOTARY_KEY_ID, APPLE_NOTARY_ISSUER and
APPLE_NOTARY_KEY_P8_BASE64 from the environment. With QAREEL_DRY_RUN=true and
no signing identity configured, it warns and leaves the binaries unsigned.
EOF
}

fail() {
    printf 'macos-sign: %s\n' "$1" >&2
    exit 1
}

[ "$#" -gt 0 ] || { usage >&2; exit 1; }
case "$1" in -h|--help) usage; exit 0 ;; esac
[ "$(uname -s)" = Darwin ] || fail 'signing runs on macOS only.'
for binary in "$@"; do
    if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
        fail "$binary is not an executable file."
    fi
done

missing=''
for name in APPLE_DEVELOPER_ID_P12_BASE64 APPLE_DEVELOPER_ID_P12_PASSWORD APPLE_SIGNING_IDENTITY \
    APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER APPLE_NOTARY_KEY_P8_BASE64; do
    eval "value=\${$name:-}"
    [ -n "$value" ] || missing="$missing $name"
done
if [ -n "$missing" ]; then
    if [ "${QAREEL_DRY_RUN:-false}" = true ]; then
        printf '::warning::Dry run without Apple signing secrets (%s); the macOS binaries are not signed or notarized.\n' "${missing# }"
        exit 0
    fi
    fail "missing signing secrets:$missing"
fi

work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/qareel-sign.XXXXXX")
keychain="$work/signing.keychain-db"
add_to_search_list() {
    set -- "$keychain"
    while IFS= read -r entry; do
        entry=${entry#"${entry%%[![:space:]]*}"}
        entry=${entry#\"}
        entry=${entry%\"}
        if [ -n "$entry" ]; then
            set -- "$@" "$entry"
        fi
    done <<EOF
$(security list-keychains -d user)
EOF
    security list-keychains -d user -s "$@"
}

cleanup() {
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

keychain_password=$(openssl rand -base64 32)
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
printf '%s' "$APPLE_DEVELOPER_ID_P12_BASE64" | base64 --decode > "$work/identity.p12"
security import "$work/identity.p12" -k "$keychain" -f pkcs12 -P "$APPLE_DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign >/dev/null
rm -f "$work/identity.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null
add_to_search_list

mkdir -p "$work/notarize"
for binary in "$@"; do
    codesign --force --options runtime --timestamp --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" "$binary"
    codesign --verify --strict --verbose=2 "$binary"
    cp "$binary" "$work/notarize/"
done

printf '%s' "$APPLE_NOTARY_KEY_P8_BASE64" | base64 --decode > "$work/notary.p8"
ditto -c -k "$work/notarize" "$work/notarize.zip"
xcrun notarytool submit "$work/notarize.zip" \
    --key "$work/notary.p8" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER" \
    --wait --timeout 45m --output-format json > "$work/notary.json" || true
status=$(plutil -extract status raw -o - "$work/notary.json" 2>/dev/null || true)
submission=$(plutil -extract id raw -o - "$work/notary.json" 2>/dev/null || true)
if [ "$status" != Accepted ]; then
    cat "$work/notary.json" >&2 || true
    if [ -n "$submission" ]; then
        xcrun notarytool log "$submission" --key "$work/notary.p8" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER" || true
    fi
    fail "notarization did not succeed (status '${status:-unknown}', submission '${submission:-none}')."
fi
printf 'Notarized submission %s\n' "$submission"
