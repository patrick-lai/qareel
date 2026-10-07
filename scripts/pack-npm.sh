#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
platforms='darwin-arm64 darwin-x64 linux-x64 linux-arm64'

usage() {
    cat <<'EOF'
Usage: scripts/pack-npm.sh [--local]

Stages the npm packages from dist/qareel-<version>-<platform>.tar.gz into
dist/npm/ and runs npm pack for each.

  --local  Pack one self-contained dist/npm/patrick-lai-qareel-<version>.tgz for this
           machine (binaries under vendor/, no optional dependencies), so
           `npx ./dist/npm/patrick-lai-qareel-<version>.tgz guide` works offline.
EOF
}

fail() {
    printf 'pack-npm: %s\n' "$1" >&2
    exit 1
}

local_mode=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --local) local_mode=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; fail "unknown option: $1" ;;
    esac
done

for tool in cargo node npm tar; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required."
done

version=$(cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" | node -e '
let input = "";
process.stdin.on("data", (chunk) => { input += chunk; }).on("end", () => {
  const found = JSON.parse(input).packages.find((item) => item.name === "qareel");
  if (found === undefined) process.exit(1);
  process.stdout.write(found.version);
});')
case "$version" in
    ''|*[!0-9A-Za-z.+-]*) fail "could not read the qareel version from Cargo (got '$version')." ;;
esac

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

dist="$root/dist"
out="$dist/npm"
mkdir -p "$dist"
stage=$(mktemp -d "$dist/.npm-stage.XXXXXX")
trap 'rm -rf "$stage"' EXIT
trap 'exit 1' INT TERM
rm -rf "$out"
mkdir -p "$out"

extract() {
    tarball="$dist/qareel-$version-$1.tar.gz"
    [ -f "$tarball" ] || fail "missing $tarball; run scripts/build-dist.sh or download the release build first."
    if [ -f "$tarball.sha256" ]; then
        expected=$(awk '{print $1}' "$tarball.sha256")
        [ "$expected" = "$(sha256 "$tarball")" ] || fail "$tarball does not match $tarball.sha256."
    fi
    unpacked="$stage/unpacked-$1"
    mkdir -p "$unpacked"
    tar -xzf "$tarball" -C "$unpacked"
    if [ ! -f "$unpacked/qareel/bin/qareel" ] || [ ! -x "$unpacked/qareel/bin/qareel" ]; then
        fail "$tarball has no executable qareel/bin/qareel."
    fi
    mkdir -p "$(dirname "$2")"
    mv "$unpacked/qareel" "$2"
}

pack() {
    (cd "$1" && npm pack --ignore-scripts --loglevel=warn --pack-destination "$out" >/dev/null)
}

main="$stage/qareel"
mkdir -p "$main/bin"
cp "$root/npm/qareel/bin/qareel.js" "$main/bin/qareel.js"
chmod 0755 "$main/bin/qareel.js"
cp "$root/npm/qareel/README.md" "$main/README.md"
cp "$root/LICENSE" "$main/LICENSE"

if [ "$local_mode" -eq 1 ]; then
    host=$(node -p 'process.platform + "-" + process.arch')
    case " $platforms " in
        *" $host "*) ;;
        *) fail "qareel has no build for $host." ;;
    esac
    extract "$host" "$main/vendor/$host"
else
    for platform in $platforms; do
        package="$stage/qareel-$platform"
        extract "$platform" "$package"
        cp "$root/LICENSE" "$package/LICENSE"
        node -e '
const fs = require("node:fs");
const [template, target, platform, version] = process.argv.slice(1);
const [os, cpu] = platform.split("-");
const text = fs.readFileSync(template, "utf8")
  .replaceAll("@PLATFORM@", platform).replaceAll("@VERSION@", version)
  .replaceAll("@OS@", os).replaceAll("@CPU@", cpu);
fs.writeFileSync(target, JSON.stringify(JSON.parse(text), null, 2) + "\n");
' "$root/npm/platform/package.json.template" "$package/package.json" "$platform" "$version"
        pack "$package"
    done
fi

node -e '
const fs = require("node:fs");
const [source, target, version, local] = process.argv.slice(1);
const manifest = JSON.parse(fs.readFileSync(source, "utf8"));
manifest.version = version;
if (local === "1") {
  delete manifest.optionalDependencies;
  manifest.files = [...manifest.files, "vendor/"];
} else {
  for (const name of Object.keys(manifest.optionalDependencies)) manifest.optionalDependencies[name] = version;
}
fs.writeFileSync(target, JSON.stringify(manifest, null, 2) + "\n");
' "$root/npm/qareel/package.json" "$main/package.json" "$version" "$local_mode"
pack "$main"

for file in "$out"/*.tgz; do
    printf '%s\n' "$file"
done
