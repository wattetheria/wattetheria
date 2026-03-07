$ErrorActionPreference = "Stop"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Error "Rust toolchain is required. Install rustup first: https://rustup.rs"
}

$RepoDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $RepoDir

cargo install --path apps/wattetheria-kernel --bin wattetheria-kernel --force
cargo install --path apps/wattetheria-cli --bin wattetheria-client-cli --force
cargo install --path apps/wattetheria-observatory --bin wattetheria-observatory --force

Write-Output "Installed binaries: wattetheria-kernel, wattetheria-client-cli, wattetheria-observatory"
Write-Output "Bootstrap a node:"
Write-Output "  wattetheria-client-cli init --data-dir .wattetheria"
Write-Output "  wattetheria-client-cli up --data-dir .wattetheria"
