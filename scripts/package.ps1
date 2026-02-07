$ErrorActionPreference = "Stop"

$RootDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$DistDir = Join-Path $RootDir "dist"
New-Item -ItemType Directory -Path $DistDir -Force | Out-Null

Set-Location $RootDir
cargo build --release -p wattetheria-kernel -p wattetheria-client-cli -p wattetheria-observatory

$osName = "windows"
$archName = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { "x86" }
$pkgName = "wattetheria-$osName-$archName"
$pkgDir = Join-Path $DistDir $pkgName
$binDir = Join-Path $pkgDir "bin"
New-Item -ItemType Directory -Path $binDir -Force | Out-Null

Copy-Item "target/release/wattetheria-kernel.exe" (Join-Path $binDir "wattetheria-kernel.exe") -Force
Copy-Item "target/release/wattetheria-client-cli.exe" (Join-Path $binDir "wattetheria-client-cli.exe") -Force
Copy-Item "target/release/wattetheria-observatory.exe" (Join-Path $binDir "wattetheria-observatory.exe") -Force
Copy-Item "README.md" (Join-Path $pkgDir "README.md") -Force

$zipPath = Join-Path $DistDir "$pkgName.zip"
if (Test-Path $zipPath) {
  Remove-Item $zipPath -Force
}
Compress-Archive -Path "$pkgDir/*" -DestinationPath $zipPath
Write-Output "Package generated under $DistDir"
