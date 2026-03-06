#!/usr/bin/env bash
set -euo pipefail

repo="${HOMEBREW_RELEASE_REPO:-bnomei/muninn}"
formula="${HOMEBREW_FORMULA_PATH:-packaging/homebrew/Formula/muninn.rb}"
asset_name="${HOMEBREW_ASSET_NAME:-muninn}"

version="${1:-}"
if [[ -z "${version}" ]]; then
  version="$(ruby -ne 'if $_ =~ /version\s+"([^"]+)"/; puts $1; exit; end' "$formula")"
fi

if [[ -z "${version}" ]]; then
  echo "Failed to determine version from ${formula}." >&2
  exit 1
fi

tag="v${version#v}"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

targets=(
  "aarch64-apple-darwin"
  "x86_64-apple-darwin"
)

sha_aarch64_apple_darwin=""
sha_x86_64_apple_darwin=""

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
    return
  fi
  echo "Missing shasum/sha256sum to compute checksums." >&2
  exit 1
}

for target in "${targets[@]}"; do
  archive="${asset_name}-v${version#v}-${target}.tar.gz"
  url="https://github.com/${repo}/releases/download/${tag}/${archive}"
  echo "Downloading ${archive}"
  curl -fsSL "$url" -o "$workdir/$archive"
  sha_value="$(sha256_file "$workdir/$archive")"
  case "$target" in
    aarch64-apple-darwin) sha_aarch64_apple_darwin="$sha_value" ;;
    x86_64-apple-darwin) sha_x86_64_apple_darwin="$sha_value" ;;
    *) echo "Unknown target: $target" >&2; exit 1 ;;
  esac
done

export SHA_AARCH64_APPLE_DARWIN="${sha_aarch64_apple_darwin}"
export SHA_X86_64_APPLE_DARWIN="${sha_x86_64_apple_darwin}"

python3 - "$formula" <<'PY'
import os
import re
import sys

formula = sys.argv[1]

checksums = {
    "aarch64_apple_darwin": os.environ["SHA_AARCH64_APPLE_DARWIN"],
    "x86_64_apple_darwin": os.environ["SHA_X86_64_APPLE_DARWIN"],
}

with open(formula, "r", encoding="utf-8") as f:
    lines = f.read().splitlines()

out = []
pattern = re.compile(r'^(\s*)([a-z0-9_]+):(\s*")([a-f0-9]{64}|REPLACE_WITH_SHA256)("\s*,?\s*)$')

for line in lines:
    m = pattern.match(line)
    if m and m.group(2) in checksums:
        line = f"{m.group(1)}{m.group(2)}:{m.group(3)}{checksums[m.group(2)]}{m.group(5)}"
    out.append(line)

with open(formula, "w", encoding="utf-8") as f:
    f.write("\n".join(out) + "\n")

print("Updated checksums in", formula)
PY

printf "\nChecksums updated for version %s:\n" "$version"
printf -- "- aarch64-apple-darwin: %s\n" "${sha_aarch64_apple_darwin}"
printf -- "- x86_64-apple-darwin: %s\n" "${sha_x86_64_apple_darwin}"
