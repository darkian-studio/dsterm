#!/bin/bash

set -euo pipefail

# Detect the correct binary name for this platform and architecture.
detect_binary_name() {
    local arch
    arch=$(uname -m)

    # Detect Termux (Android)
    if [ -n "${TERMUX_VERSION:-}" ] || [ -d "/data/data/com.termux" ]; then
        case "$arch" in
            armv7l | armv8l) echo "dsterm-android-armv7" ;;
            aarch64)          echo "dsterm-android-arm64" ;;
            x86_64)           echo "dsterm-android-x86_64" ;;
            *)
                echo "Unsupported Termux architecture: $arch. Please open a GitHub issue." >&2
                exit 1
                ;;
        esac
        return
    fi

    # Detect regular Linux
    local os
    os=$(uname -s)
    if [ "$os" = "Linux" ]; then
        case "$arch" in
            armv7l | armv8l) echo "dsterm-linux-armv7" ;;
            aarch64)          echo "dsterm-linux-arm64" ;;
            x86_64)           echo "dsterm-linux-x86_64" ;;
            *)
                echo "Unsupported Linux architecture: $arch. Please open a GitHub issue." >&2
                exit 1
                ;;
        esac
        return
    fi

    echo "Unsupported operating system: $os. Please open a GitHub issue." >&2
    exit 1
}

# Download and install the binary.
download_binary() {
    local file_name
    file_name=$(detect_binary_name)

    local base_url="https://github.com/darkian-studio/dsterm/releases/latest/download"
    local download_url="$base_url/$file_name"

    echo "Downloading $file_name..."
    if ! curl --fail -L "$download_url" -o "$file_name"; then
        echo "Download failed. URL: $download_url" >&2
        exit 1
    fi

    local install_dir="${PREFIX:-/usr/local}/bin"
    mkdir -p "$install_dir"

    mv "$file_name" "$install_dir/dsterm"
    chmod +x "$install_dir/dsterm"

    echo "Installed dsterm to $install_dir/dsterm"
    echo "Make sure '$install_dir' is in your PATH."
}

download_binary
