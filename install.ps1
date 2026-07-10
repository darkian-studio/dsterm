#Requires -Version 5
# dsterm installer for Windows (PowerShell).
#   irm https://raw.githubusercontent.com/darkian-studio/dsterm/main/install.ps1 | iex
# Downloads the prebuilt Windows binary from the latest release, falling back to
# `cargo install` when no prebuilt asset matches this architecture.

$ErrorActionPreference = 'Stop'

$Repo = 'darkian-studio/dsterm'
$InstallDir = Join-Path $env:USERPROFILE '.dsterm\bin'

function Get-AssetName {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        'AMD64' { return 'dsterm-windows-x86_64.exe' }
        default { return $null }
    }
}

function Install-Prebuilt {
    $asset = Get-AssetName
    if (-not $asset) { return $false }

    $url = "https://github.com/$Repo/releases/latest/download/$asset"
    Write-Host "Downloading $asset..."
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $dest = Join-Path $InstallDir 'dsterm.exe'
    try {
        Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
    } catch {
        Write-Warning "Prebuilt download failed: $_"
        return $false
    }
    Write-Host "Installed dsterm to $dest"
    Write-Host "Make sure '$InstallDir' is on your PATH."
    return $true
}

function Install-Cargo {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { return $false }
    Write-Host "No prebuilt binary for this architecture; building from source with cargo..."
    cargo install --git "https://github.com/$Repo" dsterm
    Write-Host "Make sure '$([IO.Path]::Combine($env:USERPROFILE, '.cargo', 'bin'))' is on your PATH."
    return $true
}

if (Install-Prebuilt) { return }
if (Install-Cargo) { return }

Write-Error @"
Could not install dsterm: no prebuilt binary for this architecture and cargo was
not found. Install Rust from https://rustup.rs and re-run, or download a binary
from https://github.com/$Repo/releases.
"@
exit 1
