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

mkdir -p artifacts
archive="artifacts/fpasobrowser-v${version}-${label}.tar.gz"
runtime_files=("$binary")
if [[ "${RUNNER_OS:-}" == "Windows" ]]; then
  for runtime in libEGL.dll libGLESv2.dll; do
    runtime_path="target/release/${runtime}"
    if [[ ! -f "$runtime_path" ]]; then
      echo "Windows ANGLE runtime was not staged: $runtime_path" >&2
      exit 1
    fi
    runtime_files+=("$runtime")
  done
fi
tar -C target/release -czf "$archive" "${runtime_files[@]}"
printf 'created %s\n' "$archive"
