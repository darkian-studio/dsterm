#!/bin/bash

set -euo pipefail

REPO="darkian-studio/dsterm"
BASE_URL="https://github.com/$REPO/releases/latest/download"

# Detect the correct prebuilt binary name for this platform and architecture.
# Prints the asset name on stdout and returns 0, or returns 1 when there is no
# prebuilt binary for this platform (the caller then falls back to cargo).
detect_binary_name() {
    local arch os
    arch=$(uname -m)

    # FIX-122: TERMUX_VERSION gating matches src/updates.rs:153 — single source via env var
    # Termux (Android). TERMUX_VERSION is the concrete signal Termux always
    # sets; a `/data/data/com.termux` path check can resolve incorrectly on
    # some hosts, so rely on the env var instead.
    if [ -n "${TERMUX_VERSION:-}" ]; then
        case "$arch" in
            armv7l | armv8l) echo "dsterm-android-armv7" ;;
            aarch64)         echo "dsterm-android-arm64" ;;
            x86_64)          echo "dsterm-android-x86_64" ;;
            *) return 1 ;;
        esac
        return 0
    fi

    os=$(uname -s)
    case "$os" in
        Linux)
            case "$arch" in
                armv7l | armv8l) echo "dsterm-linux-armv7" ;;
                aarch64)         echo "dsterm-linux-arm64" ;;
                x86_64)          echo "dsterm-linux-x86_64" ;;
                *) return 1 ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                arm64)  echo "dsterm-macos-arm64" ;;
                x86_64) echo "dsterm-macos-x86_64" ;;
                *) return 1 ;;
            esac
            ;;
        *) return 1 ;;
    esac
    return 0
}

# Pick a writable install directory, preferring Termux's $PREFIX, then a
# writable /usr/local/bin, then ~/.local/bin.
install_dir() {
    if [ -n "${PREFIX:-}" ]; then
        echo "$PREFIX/bin"
    elif [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
    else
        echo "$HOME/.local/bin"
    fi
}

# Download and install a prebuilt binary. Returns 1 when no prebuilt binary is
# available for this platform or the download fails.
install_prebuilt() {
    local file_name url dir tmp
    file_name=$(detect_binary_name) || return 1
    url="$BASE_URL/$file_name"

    echo "Downloading $file_name..."
    tmp=$(mktemp)
    if ! curl --fail -L "$url" -o "$tmp"; then
        echo "Prebuilt download failed. URL: $url" >&2
        rm -f "$tmp"
        return 1
    fi

    dir=$(install_dir)
    mkdir -p "$dir"
    mv "$tmp" "$dir/dsterm"
    chmod +x "$dir/dsterm"

    echo "Installed dsterm to $dir/dsterm"
    echo "Make sure '$dir' is in your PATH."
    return 0
}

# Fall back to building from source with cargo (works on any platform with a
# Rust toolchain, e.g. macOS/Windows arches without a prebuilt binary).
install_cargo() {
    command -v cargo >/dev/null 2>&1 || return 1
    echo "No prebuilt binary for this platform; building from source with cargo..."
    cargo install --git "https://github.com/$REPO" dsterm
}

main() {
    if install_prebuilt; then
        exit 0
    fi
    if install_cargo; then
        exit 0
    fi
    echo "Could not install dsterm: no prebuilt binary for this platform and" >&2
    echo "cargo was not found. Install Rust from https://rustup.rs and re-run," >&2
    echo "or download a binary from https://github.com/$REPO/releases." >&2
    exit 1
}

main
