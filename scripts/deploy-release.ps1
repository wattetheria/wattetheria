$ErrorActionPreference = "Stop"

[CmdletBinding()]
param(
  [string]$ComposeFile = "docker-compose.release.yml",
  [string]$TemplateEnvFile = ".env.release.example",
  [string]$EnvFile = ".env.release.local",
  [string]$ProjectName = "wattetheria-release",
  [string]$ReleaseTag,
  [switch]$ForceRefreshEnv,
  [switch]$DryRun,
  [switch]$SkipHealthChecks
)

function Require-Command {
  param([string]$Name)

  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command not found: $Name"
  }
}

function Read-EnvFile {
  param([string]$Path)

  $map = [ordered]@{}
  foreach ($line in Get-Content -Path $Path) {
    if ([string]::IsNullOrWhiteSpace($line)) {
      continue
    }
    if ($line.TrimStart().StartsWith("#")) {
      continue
    }
    $parts = $line -split "=", 2
    if ($parts.Count -ne 2) {
      continue
    }
    $map[$parts[0].Trim()] = $parts[1]
  }
  return $map
}

function Set-EnvValue {
  param(
    [string]$Path,
    [string]$Key,
    [string]$Value
  )

  $lines = [System.Collections.Generic.List[string]]::new()
  $found = $false
  foreach ($line in Get-Content -Path $Path) {
    if ($line -match "^\Q$Key\E=") {
      $lines.Add("${Key}=${Value}")
      $found = $true
    } else {
      $lines.Add($line)
    }
  }
  if (-not $found) {
    $lines.Add("${Key}=${Value}")
  }
  Set-Content -Path $Path -Value $lines
}

function Get-EnvValue {
  param(
    $Map,
    [string]$Key,
    [string]$DefaultValue
  )

  if ($Map.Contains($Key) -and -not [string]::IsNullOrWhiteSpace($Map[$Key])) {
    return $Map[$Key]
  }

  return $DefaultValue
}

function Resolve-DeployPath {
  param(
    [string]$BaseDir,
    [string]$TargetPath
  )

  if ([string]::IsNullOrWhiteSpace($TargetPath)) {
    return $null
  }

  if ([System.IO.Path]::IsPathRooted($TargetPath)) {
    return $TargetPath
  }

  return [System.IO.Path]::GetFullPath((Join-Path $BaseDir $TargetPath))
}

function Ensure-HostStateDirs {
  param(
    [string]$BaseDir,
    $Map
  )

  foreach ($key in @("WATTETHERIA_HOST_STATE_DIR", "WATTSWARM_HOST_STATE_DIR")) {
    if (-not $Map.Contains($key)) {
      continue
    }
    $resolved = Resolve-DeployPath -BaseDir $BaseDir -TargetPath $Map[$key]
    if ($null -eq $resolved) {
      continue
    }
    New-Item -ItemType Directory -Force -Path $resolved | Out-Null
  }
}

function New-StrongPassword {
  $bytes = [byte[]]::new(24)
  [System.Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
  $value = [Convert]::ToBase64String($bytes)
  $value = $value.Replace("+", "A").Replace("/", "B").TrimEnd("=")
  return $value
}

function Wait-HttpOk {
  param(
    [string]$Name,
    [string]$Url,
    [int]$MaxAttempts = 60,
    [int]$DelaySeconds = 2
  )

  for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
    try {
      $response = Invoke-WebRequest -Uri $Url -UseBasicParsing -TimeoutSec 5
      if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 300) {
        Write-Host "[ok] $Name -> $Url"
        return
      }
    } catch {
      Start-Sleep -Seconds $DelaySeconds
      continue
    }
    Start-Sleep -Seconds $DelaySeconds
  }

  throw "Timed out waiting for $Name at $Url"
}

function Invoke-Compose {
  param([string[]]$Args)

  & docker compose --project-name $ProjectName --env-file $EnvFile -f $ComposeFile @Args
  if ($LASTEXITCODE -ne 0) {
    throw "docker compose command failed: $($Args -join ' ')"
  }
}

$repoDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $repoDir

Require-Command docker

if (-not (Test-Path $ComposeFile)) {
  throw "Compose file not found: $ComposeFile"
}

if (-not (Test-Path $TemplateEnvFile)) {
  throw "Template env file not found: $TemplateEnvFile"
}

try {
  & docker compose version | Out-Null
} catch {
  throw "Docker Compose v2 is required."
}

try {
  & docker info | Out-Null
} catch {
  throw "Docker daemon is not reachable. Start Docker Desktop or the Docker service first."
}

if ($ForceRefreshEnv -or -not (Test-Path $EnvFile)) {
  Copy-Item -Path $TemplateEnvFile -Destination $EnvFile -Force
  Write-Host "Prepared env file: $EnvFile"
}

$envMap = Read-EnvFile -Path $EnvFile
Set-EnvValue -Path $EnvFile -Key "WATTETHERIA_RUNTIME_ENV_FILE" -Value (Split-Path -Leaf $EnvFile)
$envMap["WATTETHERIA_RUNTIME_ENV_FILE"] = Split-Path -Leaf $EnvFile

$passwordPlaceholder = "replace-with-strong-password"
if (-not $envMap.Contains("WATTSWARM_PG_PASSWORD") -or
    [string]::IsNullOrWhiteSpace($envMap["WATTSWARM_PG_PASSWORD"]) -or
    $envMap["WATTSWARM_PG_PASSWORD"] -eq $passwordPlaceholder) {
  $generatedPassword = New-StrongPassword
  Set-EnvValue -Path $EnvFile -Key "WATTSWARM_PG_PASSWORD" -Value $generatedPassword
  Write-Host "Generated WATTSWARM_PG_PASSWORD in $EnvFile"
  $envMap["WATTSWARM_PG_PASSWORD"] = $generatedPassword
}

if ($ReleaseTag) {
  $imageKeys = @(
    "WATTETHERIA_KERNEL_IMAGE",
    "WATTETHERIA_OBSERVATORY_IMAGE",
    "WATTSWARM_KERNEL_IMAGE",
    "WATTSWARM_RUNTIME_IMAGE",
    "WATTSWARM_WORKER_IMAGE"
  )
  foreach ($key in $imageKeys) {
    if (-not $envMap.Contains($key)) {
      continue
    }
    $current = $envMap[$key]
    $updated = if ($current -match "^(.*:).+$") {
      "$($Matches[1])$ReleaseTag"
    } else {
      "${current}:$ReleaseTag"
    }
    Set-EnvValue -Path $EnvFile -Key $key -Value $updated
    $envMap[$key] = $updated
  }
  Write-Host "Pinned release image tags to: $ReleaseTag"
}

Ensure-HostStateDirs -BaseDir $repoDir -Map $envMap

Invoke-Compose -Args @("config") | Out-Null

if ($DryRun) {
  Write-Host "Dry run complete."
  Write-Host "Compose file: $ComposeFile"
  Write-Host "Env file: $EnvFile"
  return
}

Write-Host "Pulling release images..."
Invoke-Compose -Args @("pull")

Write-Host "Starting release stack..."
Invoke-Compose -Args @("up", "-d")

if (-not $SkipHealthChecks) {
  $envMap = Read-EnvFile -Path $EnvFile
  $kernelHost = Get-EnvValue -Map $envMap -Key "WATTETHERIA_CONTROL_PLANE_BIND_HOST" -DefaultValue "127.0.0.1"
  $kernelPort = Get-EnvValue -Map $envMap -Key "WATTETHERIA_CONTROL_PLANE_PORT" -DefaultValue "7777"
  $uiHost = Get-EnvValue -Map $envMap -Key "WATTSWARM_UI_BIND_HOST" -DefaultValue "127.0.0.1"
  $uiPort = Get-EnvValue -Map $envMap -Key "WATTSWARM_UI_PORT" -DefaultValue "7788"
  $observatoryHost = Get-EnvValue -Map $envMap -Key "WATTETHERIA_OBSERVATORY_BIND_HOST" -DefaultValue "127.0.0.1"
  $observatoryPort = Get-EnvValue -Map $envMap -Key "WATTETHERIA_OBSERVATORY_PORT" -DefaultValue "8780"

  Wait-HttpOk -Name "kernel health" -Url "http://${kernelHost}:${kernelPort}/v1/health"
  Wait-HttpOk -Name "wattswarm ui" -Url "http://${uiHost}:${uiPort}/"
  Wait-HttpOk -Name "observatory health" -Url "http://${observatoryHost}:${observatoryPort}/healthz"
}

$kernelHost = Get-EnvValue -Map $envMap -Key "WATTETHERIA_CONTROL_PLANE_BIND_HOST" -DefaultValue "127.0.0.1"
$kernelPort = Get-EnvValue -Map $envMap -Key "WATTETHERIA_CONTROL_PLANE_PORT" -DefaultValue "7777"
$uiHost = Get-EnvValue -Map $envMap -Key "WATTSWARM_UI_BIND_HOST" -DefaultValue "127.0.0.1"
$uiPort = Get-EnvValue -Map $envMap -Key "WATTSWARM_UI_PORT" -DefaultValue "7788"
$observatoryHost = Get-EnvValue -Map $envMap -Key "WATTETHERIA_OBSERVATORY_BIND_HOST" -DefaultValue "127.0.0.1"
$observatoryPort = Get-EnvValue -Map $envMap -Key "WATTETHERIA_OBSERVATORY_PORT" -DefaultValue "8780"

Write-Host ""
Write-Host "Deployment complete."
Write-Host "Kernel:      http://${kernelHost}:${kernelPort}"
Write-Host "Wattswarm UI:http://${uiHost}:${uiPort}"
Write-Host "Observatory: http://${observatoryHost}:${observatoryPort}"
Write-Host "Env file:    $EnvFile"
