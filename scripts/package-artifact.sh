#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <platform-label>" >&2
  exit 64
fi

label="$1"
version="$(awk -F '"' '$1 ~ /^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)"
if [[ -z "$version" ]]; then
  echo "could not read package version from Cargo.toml" >&2
  exit 1
fi

binary="fpasobrowser"
if [[ "${RUNNER_OS:-}" == "Windows" ]]; then
  binary+=".exe"
fi

binary_path="target/release/${binary}"
if [[ ! -f "$binary_path" ]]; then
  echo "release binary not found: $binary_path" >&2
  exit 1
fi

bash scripts/collect-third-party-licenses.sh

mkdir -p artifacts
archive="artifacts/fpasobrowser-v${version}-${label}.tar.gz"
runtime_files=("$binary")
windows_launcher=''
if [[ "${RUNNER_OS:-}" == "Windows" ]]; then
  for runtime in libEGL.dll libGLESv2.dll; do
    runtime_path="target/release/${runtime}"
    if [[ ! -f "$runtime_path" ]]; then
      echo "Windows ANGLE runtime was not staged: $runtime_path" >&2
      exit 1
    fi
    runtime_files+=("$runtime")
  done
  if [[ ! -f fpasobrowser.cmd ]]; then
    echo "Windows launcher was not found: fpasobrowser.cmd" >&2
    exit 1
  fi
  windows_launcher='fpasobrowser.cmd'
fi
package_files=("${runtime_files[@]}")
if [[ -n "$windows_launcher" ]]; then
  package_files+=("$windows_launcher")
fi
staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/fpasobrowser-package.XXXXXX")"
trap 'rm -rf -- "$staging_dir"' EXIT
for runtime_file in "${runtime_files[@]}"; do
  cp "target/release/$runtime_file" "$staging_dir/"
done
if [[ "${RUNNER_OS:-}" == "Windows" ]]; then
  cp fpasobrowser.cmd "$staging_dir/"
fi
cp LICENSE THIRD_PARTY_NOTICES.md Cargo.lock "$staging_dir/"
cp -R target/release/THIRD_PARTY_LICENSES "$staging_dir/"
tar -C "$staging_dir" -czf "$archive" \
  "${package_files[@]}" LICENSE THIRD_PARTY_NOTICES.md Cargo.lock THIRD_PARTY_LICENSES
printf 'created %s\n' "$archive"
