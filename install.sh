#!/bin/bash

# detect architecture
detect_arch() {
    case $(uname -m) in
        armv7l | armv8l)
            echo "android-armv7"
            ;;
        aarch64)
            echo "android-arm64"
            ;;
        x86_64)
            echo "android-x86_64"
            ;;
        *)
            echo "Unsupported architecture. Please create an issue on GitHub, and we will consider providing a binary for your architecture."
            exit 1
            ;;
    esac
}

# download the appropriate binary
download_binary() {
    ARCH=$(detect_arch)
    BASE_URL="https://github.com/darkian-studio/dsterm/releases/latest/download"

    FILE_NAME="dsterm-$ARCH"
    DOWNLOAD_URL="$BASE_URL/$FILE_NAME"

    # Download the binary
    echo "Downloading $FILE_NAME for $ARCH architecture..."
    if ! curl --progress-bar --fail -L "$DOWNLOAD_URL" -o "$FILE_NAME"; then
        echo "Failed to download the binary! Please check the URL and your connection: $DOWNLOAD_URL"
        exit 1
    fi

    # Move the binary to the PREFIX directory and rename it to 'dsterm'
    echo "Installing dsterm binary to $PREFIX..."
    mv "$FILE_NAME" "$PREFIX/bin/dsterm"
    chmod +x "$PREFIX/bin/dsterm"

    # Create a symlink acodeX-server pointing to dsterm
    ln -sf "$PREFIX/bin/dsterm" "$PREFIX/bin/acodeX-server"

    echo "Binary downloaded and installed as 'dsterm'. You can now use the 'dsterm' command!"
    echo "Make sure '$PREFIX/bin' is in your PATH."
}

download_binary
