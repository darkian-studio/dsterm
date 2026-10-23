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

# #73 fail closed: the release must publish a checksum asset and the
# download must match it. Missing/malformed/mismatch aborts the install -
# never a silent skip, never a cargo fallback after a failed verification.
function Test-ReleaseChecksum {
    param([string]$Asset, [string]$FilePath)

    $sumUrl = "https://github.com/$Repo/releases/latest/download/$Asset.sha256"
    try {
        $sumText = [string](Invoke-WebRequest -Uri $sumUrl -UseBasicParsing).Content
    } catch {
        throw "release publishes no checksum for $Asset (fail closed): $($_.Exception.Message)"
    }
    $expected = ($sumText -split '\s+')[0].ToLowerInvariant()
    if ($expected -notmatch '^[0-9a-f]{64}$') {
        throw "malformed checksum asset for $Asset (fail closed)"
    }
    $actual = (Get-FileHash -Path $FilePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "checksum mismatch for $Asset (fail closed): expected $expected, got $actual"
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
    try {
        Test-ReleaseChecksum -Asset $asset -FilePath $dest
    } catch {
        # Leave nothing unverified behind, then abort the whole script
        # (terminating error -> script exits non-zero, no cargo fallback).
        Remove-Item -Force $dest -ErrorAction SilentlyContinue
        throw "Prebuilt verification failed: $($_.Exception.Message)"
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
