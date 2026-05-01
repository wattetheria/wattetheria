$ErrorActionPreference = "Stop"

$RootDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$DistDir = Join-Path $RootDir "dist"
New-Item -ItemType Directory -Path $DistDir -Force | Out-Null

Set-Location $RootDir
cargo build --release -p wattetheria-kernel -p wattetheria-client-cli

$osName = "windows"
$archName = if ([Environment]::Is64BitOperatingSystem) { "x86_64" } else { "x86" }
$pkgName = "wattetheria-$osName-$archName"
$pkgDir = Join-Path $DistDir $pkgName
$binDir = Join-Path $pkgDir "bin"
New-Item -ItemType Directory -Path $binDir -Force | Out-Null

Copy-Item "target/release/wattetheria-kernel.exe" (Join-Path $binDir "wattetheria-kernel.exe") -Force
Copy-Item "target/release/wattetheria-client-cli.exe" (Join-Path $binDir "wattetheria-client-cli.exe") -Force
Copy-Item "README.md" (Join-Path $pkgDir "README.md") -Force
Copy-Item "docker-compose.release.yml" (Join-Path $pkgDir "docker-compose.release.yml") -Force
Copy-Item ".env.release" (Join-Path $pkgDir ".env.release") -Force
New-Item -ItemType Directory -Path (Join-Path $pkgDir "scripts") -Force | Out-Null
Copy-Item "scripts/deploy-release.ps1" (Join-Path $pkgDir "scripts/deploy-release.ps1") -Force
Copy-Item "docs/dev/RELEASE_PUBLISH_CHECKLIST.md" (Join-Path $pkgDir "RELEASE_PUBLISH_CHECKLIST.md") -Force

$zipPath = Join-Path $DistDir "$pkgName.zip"
if (Test-Path $zipPath) {
  Remove-Item $zipPath -Force
}
Compress-Archive -Path "$pkgDir/*" -DestinationPath $zipPath
Write-Output "Package generated under $DistDir"
