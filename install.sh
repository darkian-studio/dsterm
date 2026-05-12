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

# progress bar function
show_progress() {
    local current=$1
    local total=$2
    local width=40
    
    if [ "$total" -le 0 ]; then
        return
    fi
    
    local percent=$((current * 100 / total))
    local filled=$((percent * width / 100))
    
    printf "\r["
    printf "%${filled}s" | tr ' ' '='
    printf "%$((width - filled))s" | tr ' ' '-'
    printf "] %d%%" "$percent"
}

# download the appropriate binary
download_binary() {
    ARCH=$(detect_arch)
    BASE_URL="https://github.com/darkian-studio/dsterm/releases/latest/download"

    FILE_NAME="dsterm-$ARCH"
    DOWNLOAD_URL="$BASE_URL/$FILE_NAME"

    # Download the binary with progress bar
    echo "Downloading $FILE_NAME for $ARCH architecture..."
    if ! curl -L "$DOWNLOAD_URL" -o "$FILE_NAME" -w "\n" --progress-bar 2>&1 | while IFS= read -r line; do
        if [[ $line =~ ([0-9]+\.[0-9]|[0-9]+)% ]]; then
            percent="${BASH_REMATCH[1]%\.*}"
            show_progress "$percent" "100"
        fi
    done; then
        printf "\n"
        echo "Failed to download the binary! Please check the URL and your connection: $DOWNLOAD_URL"
        exit 1
    fi
    printf "\n"

    # Move the binary to the PREFIX directory and rename it to 'dsterm'
    echo "Installing dsterm binary to $PREFIX..."
    mv "$FILE_NAME" "$PREFIX/bin/dsterm"
    chmod +x "$PREFIX/bin/dsterm"

    echo "Binary downloaded and installed as 'dsterm'. You can now use the 'dsterm' command!"
    echo "Make sure '$PREFIX/bin' is in your PATH."
}

download_binary
