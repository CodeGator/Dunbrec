<#
.SYNOPSIS
  Build the Dunbrec release executable and its Windows installer (.msi).

.DESCRIPTION
  Requires the WiX Toolset v4 or later on PATH (`dotnet tool install --global wix`).
  Output: target\installer\Dunbrec-<version>-x64.msi

.PARAMETER Manufacturer
  Publisher name shown in Windows' installed-apps list.
#>
param([string]$Manufacturer = "Dunbrec")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host "Building release executable..."
cargo build --release -p dunbrec-app
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# Version from the workspace manifest (MSI versions are major.minor.patch).
$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq "dunbrec-app" } | Select-Object -ExpandProperty version
$version = ($version -split "[-+]")[0]

$out = Join-Path $root "target\installer"
New-Item -ItemType Directory -Force $out | Out-Null
$msi = Join-Path $out "Dunbrec-$version-x64.msi"

Write-Host "Building installer $msi ..."
wix build installer\dunbrec.wxs -arch x64 `
    -d "Version=$version" `
    -d "Manufacturer=$Manufacturer" `
    -d "BinDir=$(Join-Path $root 'target\release')" `
    -d "AssetsDir=$(Join-Path $root 'assets')" `
    -d "RootDir=$root" `
    -o $msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
Write-Host "Done: $msi"
