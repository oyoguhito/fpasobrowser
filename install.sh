#!/usr/bin/env bash
# Install the native source-build prerequisites for this host and build the
# local fpasobrowser binary. Run from a source checkout:
#   ./install.sh
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
build=true
install_prerequisites=true

usage() {
    cat <<'EOF'
Usage: ./install.sh [--build-only] [--no-build]

Install fpasobrowser source-build prerequisites for Linux or macOS, then build
target/debug/fpasobrowser. --build-only skips OS package installation and uses
the normal prerequisite-aware launcher. --no-build installs prerequisites only.
EOF
}

for argument in "$@"; do
    case "$argument" in
        --build-only) install_prerequisites=false ;;
        --no-build) build=false ;;
        --help|-h) usage; exit 0 ;;
        *) echo "fpasobrowser install: unknown option: $argument" >&2; usage >&2; exit 64 ;;
    esac
done

as_root() {
    if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        echo 'fpasobrowser install: this package installation needs root (run with sudo or install sudo).' >&2
        exit 77
    fi
}

install_rust() {
    if command -v cargo >/dev/null 2>&1 || [[ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]]; then
        return
    fi
    if ! command -v curl >/dev/null 2>&1; then
        echo 'fpasobrowser install: Cargo is missing and curl is unavailable for rustup.' >&2
        exit 127
    fi
    local rustup_installer
    rustup_installer="$(mktemp)"
    trap 'rm -f "$rustup_installer"' EXIT
    echo 'fpasobrowser install: installing Rust stable with rustup...' >&2
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$rustup_installer"
    sh "$rustup_installer" -y --profile minimal
    rm -f "$rustup_installer"
    trap - EXIT
}

install_linux_prerequisites() {
    if command -v apt-get >/dev/null 2>&1; then
        as_root apt-get update
        as_root apt-get install -y build-essential pkg-config libfontconfig1-dev clang mold python3 curl
    elif command -v dnf >/dev/null 2>&1; then
        as_root dnf install -y gcc-c++ pkgconf-pkg-config fontconfig-devel clang mold python3 curl
    elif command -v pacman >/dev/null 2>&1; then
        as_root pacman -Sy --needed --noconfirm base-devel pkgconf fontconfig clang mold python curl
    else
        echo 'fpasobrowser install: supported Linux package managers are apt, dnf, and pacman.' >&2
        echo 'Install Python 3, clang, mold, pkg-config, fontconfig development files, curl, and Rust stable, then run ./install.sh --build-only.' >&2
        exit 69
    fi
    install_rust
}

install_macos_prerequisites() {
    if ! xcode-select -p >/dev/null 2>&1; then
        echo 'fpasobrowser install: installing Xcode Command Line Tools; rerun this command after macOS finishes the installation.' >&2
        xcode-select --install
        exit 75
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        if command -v brew >/dev/null 2>&1; then
            brew install python
        else
            echo 'fpasobrowser install: Python 3 is missing. Install it (for example: brew install python), then rerun this command.' >&2
            exit 127
        fi
    fi
    install_rust
}

if [[ "$install_prerequisites" == true ]]; then
    case "$(uname -s)" in
        Linux) install_linux_prerequisites ;;
        Darwin) install_macos_prerequisites ;;
        *)
            echo "fpasobrowser install: unsupported host $(uname -s). Use bin/fpasobrowser --doctor and install the listed prerequisites." >&2
            exit 69
            ;;
    esac
fi

if [[ "$build" == true ]]; then
    exec "$script_dir/bin/fpasobrowser" --build
fi

echo 'fpasobrowser install: prerequisites are ready; run ./bin/fpasobrowser --build when ready to compile.' >&2
