#!/usr/bin/env pwsh
# Build the release binary and package it (binary + README + LICENSE) as a zip
# in dist/, with a matching SHA-256 checksum. Packages the current host only.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Repo root is the parent of this script's directory.
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $cargo = Get-Content Cargo.toml -Raw
    if ($cargo -notmatch '(?m)^name\s*=\s*"([^"]+)"') {
        throw 'could not read the package name from Cargo.toml'
    }
    $pkg = $Matches[1]
    if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
        throw 'could not read the version from Cargo.toml'
    }
    $version = $Matches[1]

    $triple = ((rustc -vV) | Select-String '^host: ').ToString().Split(':')[1].Trim()

    Write-Host "Building $pkg $version for $triple..."
    cargo build --release --locked

    $exe = if ($triple -like '*windows*') { "$pkg.exe" } else { $pkg }
    $binary = Join-Path $root "target/release/$exe"
    if (-not (Test-Path $binary)) {
        throw "missing build output: $binary"
    }

    $stem = "$pkg-$version-$triple"
    $dist = Join-Path $root 'dist'
    $stage = Join-Path $dist $stem
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $stage -Force | Out-Null

    Copy-Item $binary $stage
    foreach ($doc in 'README.md', 'LICENSE') {
        if (Test-Path (Join-Path $root $doc)) {
            Copy-Item (Join-Path $root $doc) $stage
        }
    }

    $zip = Join-Path $dist "$stem.zip"
    Remove-Item $zip -Force -ErrorAction SilentlyContinue
    # Zipping the staging *directory* puts it as the archive's top-level folder.
    Compress-Archive -Path $stage -DestinationPath $zip

    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    # LF and no BOM so the file is valid for `sha256sum -c` on unix too.
    [IO.File]::WriteAllText((Join-Path $dist "$stem.zip.sha256"), "$hash  $stem.zip`n")

    Remove-Item $stage -Recurse -Force
    Write-Host "Wrote $zip"
}
finally {
    Pop-Location
}
