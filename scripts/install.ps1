<#
.SYNOPSIS
  Pawrly installer for Windows. Downloads a prebuilt `pawrly.exe` and installs it.

.DESCRIPTION
  Run with:
    irm https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.ps1 | iex

  Environment overrides:
    $env:PAWRLY_VERSION     Tag to install (e.g. v0.1.0). Default: latest release.
    $env:PAWRLY_INSTALL_DIR Install directory. Default: $env:LOCALAPPDATA\Pawrly\bin
    $env:PAWRLY_REPO        owner/repo. Default: CITGuru/pawrly
    $env:PAWRLY_NO_VERIFY   Set to 1 to skip checksum verification.

  Note: Pawrly currently publishes prebuilt binaries for Linux and macOS only.
  On Windows, prefer WSL, or build from source with `cargo install`.
#>

$ErrorActionPreference = "Stop"

$Repo    = if ($env:PAWRLY_REPO) { $env:PAWRLY_REPO } else { "CITGuru/pawrly" }
$BinName = "pawrly"

function Get-Target {
  $arch = $env:PROCESSOR_ARCHITECTURE
  switch ($arch) {
    "AMD64" { return "x86_64-pc-windows-msvc" }
    "ARM64" { return "aarch64-pc-windows-msvc" }
    default { throw "Unsupported architecture: $arch" }
  }
}

function Get-LatestVersion {
  $api = "https://api.github.com/repos/$Repo/releases/latest"
  $resp = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "pawrly-installer" }
  if (-not $resp.tag_name) { throw "Could not determine latest release from $api" }
  return $resp.tag_name
}

$target  = Get-Target
$version = if ($env:PAWRLY_VERSION) { $env:PAWRLY_VERSION } else { Get-LatestVersion }
$dir     = if ($env:PAWRLY_INSTALL_DIR) { $env:PAWRLY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Pawrly\bin" }

$zip     = "$BinName-$target.zip"
$baseUrl = "https://github.com/$Repo/releases/download/$version"
$url     = "$baseUrl/$zip"

Write-Host "pawrly: installing $version for $target" -ForegroundColor Cyan

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("pawrly-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $zipPath = Join-Path $tmp $zip
  Write-Host "pawrly: downloading $url"
  try {
    Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing
  } catch {
    Write-Warning "No prebuilt Windows binary found at $version."
    Write-Host  "Build from source instead:  cargo install --git https://github.com/$Repo pawrly-cli"
    throw
  }

  if ($env:PAWRLY_NO_VERIFY -ne "1") {
    try {
      $sumPath = "$zipPath.sha256"
      Invoke-WebRequest -Uri "$url.sha256" -OutFile $sumPath -UseBasicParsing
      $expected = (Get-Content $sumPath | Select-Object -First 1).Split(" ")[0]
      $actual   = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
      if ($expected.ToLower() -ne $actual) {
        throw "Checksum mismatch — expected $expected, got $actual"
      }
      Write-Host "pawrly: checksum verified"
    } catch {
      Write-Warning "Skipping checksum verification: $_"
    }
  }

  Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  Copy-Item -Path (Join-Path $tmp "$BinName.exe") -Destination (Join-Path $dir "$BinName.exe") -Force

  Write-Host "pawrly: installed $(Join-Path $dir "$BinName.exe")" -ForegroundColor Green

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$dir*") {
    Write-Warning "$dir is not on your PATH."
    Write-Host  "Add it with:  setx PATH `"$dir;`$env:PATH`""
  }
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
