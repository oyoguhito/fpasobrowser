#!/usr/bin/env bash
set -euo pipefail

# Collect license and notice files from every registry crate resolved by the
# lockfile.  This runs after Cargo has fetched the dependencies, and its output
# is copied into each binary release archive by package-artifact.sh.

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root_dir"

output_dir="target/release/THIRD_PARTY_LICENSES"
cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
registry_source_dir="$cargo_home/registry/src"

if [[ ! -d "$registry_source_dir" ]]; then
  echo "Cargo registry sources were not found: $registry_source_dir" >&2
  echo "Run cargo build --locked --release before collecting notices." >&2
  exit 1
fi

rm -rf -- "$output_dir"
mkdir -p "$output_dir"

manifest="$output_dir/README.md"
cat > "$manifest" <<'EOF'
# Third-party license files

Generated from the `Cargo.lock` that accompanies this release.  Each directory
contains license, notice, copying, and copyright files found in the resolved
crate source.  The package name and version are encoded in its directory name.
When a crate publishes its license expression only in `Cargo.toml`, that
manifest is retained instead.
EOF

missing=0
while IFS=$'\t' read -r crate_name crate_version; do
  source_dir=""
  for registry in "$registry_source_dir"/*; do
    candidate="$registry/$crate_name-$crate_version"
    if [[ -d "$candidate" ]]; then
      source_dir="$candidate"
      break
    fi
  done

  if [[ -z "$source_dir" ]]; then
    echo "missing Cargo source for $crate_name $crate_version" >&2
    missing=1
    continue
  fi

  destination="$output_dir/$crate_name-$crate_version"
  mkdir -p "$destination"
  copied=0
  while IFS= read -r -d '' source_file; do
    relative_path="${source_file#"$source_dir"/}"
    target_file="$destination/$relative_path"
    mkdir -p "$(dirname "$target_file")"
    cp "$source_file" "$target_file"
    copied=1
  done < <(find "$source_dir" -maxdepth 5 -type f \( \
    -iname 'LICENSE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' -o -iname 'COPYRIGHT*' \
  \) -print0)

  if [[ "$copied" -eq 0 ]]; then
    # A few crates intentionally publish only a SPDX expression in their
    # manifest.  Retain that exact declaration rather than silently omitting
    # the package from the archive.
    cp "$source_dir/Cargo.toml" "$destination/Cargo.toml"
  fi
done < <(
  awk '
    function emit() {
      if (name != "" && version != "" && source ~ /^registry\+/) {
        print name "\t" version
      }
    }
    /^\[\[package\]\]$/ { emit(); name = version = source = ""; next }
    $1 == "name" { value = $3; gsub(/^"|"$/, "", value); name = value; next }
    $1 == "version" { value = $3; gsub(/^"|"$/, "", value); version = value; next }
    $1 == "source" { value = $3; gsub(/^"|"$/, "", value); source = value; next }
    END { emit() }
  ' Cargo.lock | sort -u
)

if [[ "$missing" -ne 0 ]]; then
  rm -rf -- "$output_dir"
  exit 1
fi

printf 'collected third-party license files in %s\n' "$output_dir"
