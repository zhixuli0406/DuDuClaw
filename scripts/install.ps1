# DuDuClaw Installer for Windows
# Usage: irm https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.ps1 | iex

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
# Version is auto-detected from the latest GitHub Release at runtime so this
# script (served from `main`) never drifts behind published binaries. Override
# with $env:DUDUCLAW_VERSION to pin. Fallback is used only if the API is
# unreachable. NOTE: never hardcode a version lacking a published Release — a
# 404 here silently forces an hour-long MSVC + cargo source build.
$DuDuClawVersion = if ($env:DUDUCLAW_VERSION) { $env:DUDUCLAW_VERSION } else { "" }
$FallbackVersion = "1.36.0"
$GitHubRepo = "zhixuli0406/DuDuClaw"
$InstallDir = if ($env:DUDUCLAW_HOME) { "$env:DUDUCLAW_HOME\bin" } else { "$env:USERPROFILE\.duduclaw\bin" }
$BinaryName = "duduclaw.exe"
$MinPythonMajor = 3
$MinPythonMinor = 10

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
function Write-Info    { param([string]$Message) Write-Host "  [OK] " -ForegroundColor Green -NoNewline; Write-Host $Message }
function Write-Warn    { param([string]$Message) Write-Host "  [!!] " -ForegroundColor Yellow -NoNewline; Write-Host $Message }
function Write-Err     { param([string]$Message) Write-Host "  [XX] " -ForegroundColor Red -NoNewline; Write-Host $Message }
function Write-Heading { param([string]$Message) Write-Host ""; Write-Host "  $Message" -ForegroundColor Cyan }

# ---------------------------------------------------------------------------
# Resolve version from the latest GitHub Release (unless pinned)
# ---------------------------------------------------------------------------
function Resolve-Version {
    if ($DuDuClawVersion) {
        Write-Info "Using pinned version v$DuDuClawVersion"
        return
    }
    $api = "https://api.github.com/repos/$GitHubRepo/releases/latest"
    try {
        $resp = Invoke-RestMethod -Uri $api -UseBasicParsing -Headers @{ "User-Agent" = "duduclaw-installer" } -ErrorAction Stop
        $tag = $resp.tag_name
        if ($tag) {
            $script:DuDuClawVersion = $tag -replace '^v', ''
        }
    } catch {
        # fall through to fallback below
    }
    if (-not $DuDuClawVersion) {
        $script:DuDuClawVersion = $FallbackVersion
        Write-Warn "Could not query latest release; falling back to v$DuDuClawVersion"
    }
}

# ---------------------------------------------------------------------------
# Detect architecture
# ---------------------------------------------------------------------------
function Get-Target {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch) {
        "X64"   { return "duduclaw-windows-x64" }
        "Arm64" { return "duduclaw-windows-arm64" }
        default { throw "Unsupported architecture: $arch" }
    }
}

# ---------------------------------------------------------------------------
# Download binary from GitHub release
# ---------------------------------------------------------------------------
function Install-FromRelease {
    param([string]$Target)

    $archiveUrl = "https://github.com/$GitHubRepo/releases/download/v$DuDuClawVersion/$Target.zip"
    $shaUrl = "$archiveUrl.sha256"
    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "duduclaw-install-$(Get-Random)"

    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

    Write-Heading "Downloading DuDuClaw v$DuDuClawVersion ($Target)..."
    Write-Host "  URL: $archiveUrl"

    try {
        $archivePath = Join-Path $tempDir "$Target.zip"
        Invoke-WebRequest -Uri $archiveUrl -OutFile $archivePath -UseBasicParsing -ErrorAction Stop

        # Verify checksum if available
        try {
            $shaPath = Join-Path $tempDir "$Target.zip.sha256"
            Invoke-WebRequest -Uri $shaUrl -OutFile $shaPath -UseBasicParsing -ErrorAction Stop
            $expectedHash = (Get-Content $shaPath).Split(" ")[0].Trim()
            $actualHash = (Get-FileHash $archivePath -Algorithm SHA256).Hash
            if ($expectedHash -ieq $actualHash) {
                Write-Info "Checksum verified"
            } else {
                Write-Warn "Checksum mismatch"
            }
        } catch {
            Write-Warn "Checksum file not available, skipping verification"
        }

        # Extract
        Write-Heading "Installing..."
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Expand-Archive -Path $archivePath -DestinationPath $tempDir -Force
        Copy-Item (Join-Path $tempDir $BinaryName) (Join-Path $InstallDir $BinaryName) -Force

        Write-Info "Installed to $InstallDir\$BinaryName"
        return $true
    } catch {
        return $false
    } finally {
        Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
# Build from source
# ---------------------------------------------------------------------------
function Install-FromSource {
    Write-Heading "Building from source with cargo..."

    $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargoCmd) {
        Write-Err "cargo is not installed."
        Write-Host ""
        Write-Host "  Install Rust via: https://rustup.rs"
        Write-Host ""
        throw "Cannot build from source without cargo."
    }

    Write-Info "Found cargo: $(cargo --version)"

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

    $rootDir = if ($env:DUDUCLAW_HOME) { $env:DUDUCLAW_HOME } else { "$env:USERPROFILE\.duduclaw" }

    & cargo install `
        --git "https://github.com/$GitHubRepo.git" `
        --tag "v$DuDuClawVersion" `
        --root $rootDir `
        --locked `
        duduclaw-cli

    if ($LASTEXITCODE -ne 0) {
        Write-Warn "Tagged release v$DuDuClawVersion not found, trying main branch..."
        & cargo install `
            --git "https://github.com/$GitHubRepo.git" `
            --branch main `
            --root $rootDir `
            duduclaw-cli

        if ($LASTEXITCODE -ne 0) {
            throw "Failed to build from source."
        }
    }

    Write-Info "Built and installed to $InstallDir\$BinaryName"
}

# ---------------------------------------------------------------------------
# Add to PATH
# ---------------------------------------------------------------------------
function Add-ToPath {
    Write-Heading "Checking PATH..."

    $currentPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    if ($currentPath -split ";" | Where-Object { $_ -eq $InstallDir }) {
        Write-Info "Already in PATH"
        return
    }

    Write-Heading "Adding to user PATH..."
    $newPath = "$InstallDir;$currentPath"
    [System.Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    $env:Path = "$InstallDir;$env:Path"

    if ($env:DUDUCLAW_HOME) {
        [System.Environment]::SetEnvironmentVariable("DUDUCLAW_HOME", $env:DUDUCLAW_HOME, "User")
    }

    Write-Info "Updated user PATH"
}

# ---------------------------------------------------------------------------
# Check optional dependencies
# ---------------------------------------------------------------------------
function Test-Python {
    Write-Heading "Checking Python..."

    $pyCmd = Get-Command python -ErrorAction SilentlyContinue
    if (-not $pyCmd) {
        $pyCmd = Get-Command python3 -ErrorAction SilentlyContinue
    }

    if (-not $pyCmd) {
        Write-Warn "Python not found. Python $MinPythonMajor.$MinPythonMinor+ is recommended."
        Write-Host "  Install Python: https://www.python.org/downloads/"
        return
    }

    $pyVersion = & $pyCmd.Source -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>$null
    if ($pyVersion) {
        $parts = $pyVersion.Split(".")
        $major = [int]$parts[0]
        $minor = [int]$parts[1]

        if ($major -ge $MinPythonMajor -and $minor -ge $MinPythonMinor) {
            Write-Info "Python $pyVersion found"
            Write-Host ""
            Write-Host "  Install the Python SDK with:"
            Write-Host "    pip install duduclaw"
        } else {
            Write-Warn "Python $pyVersion found, but $MinPythonMajor.$MinPythonMinor+ is recommended."
            Write-Host "  Upgrade Python: https://www.python.org/downloads/"
        }
    }
}

function Test-Docker {
    Write-Heading "Checking Docker..."

    $dockerCmd = Get-Command docker -ErrorAction SilentlyContinue
    if ($dockerCmd) {
        $dockerVersion = & docker --version 2>$null
        Write-Info "Docker found: $dockerVersion"
    } else {
        Write-Warn "Docker Desktop not found. Docker is optional but recommended."
        Write-Host "  Install Docker Desktop: https://docs.docker.com/desktop/install/windows-install/"
    }
}

function Test-WSL {
    Write-Heading "Checking WSL2..."

    $wslCmd = Get-Command wsl -ErrorAction SilentlyContinue
    if ($wslCmd) {
        try {
            $wslStatus = & wsl --status 2>$null
            if ($LASTEXITCODE -eq 0) {
                Write-Info "WSL2 is available"
            } else {
                Write-Warn "WSL is installed but may not be configured."
            }
        } catch {
            Write-Warn "WSL check failed."
        }
    } else {
        Write-Warn "WSL2 not found. WSL2 is optional but useful for Linux-based agents."
        Write-Host "  Install WSL2: https://learn.microsoft.com/en-us/windows/wsl/install"
    }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
function Main {
    Write-Host ""
    Resolve-Version
    Write-Host "  DuDuClaw Installer v$DuDuClawVersion" -ForegroundColor White
    Write-Host "  ======================================"

    # Detect platform
    Write-Heading "Detecting platform..."
    $target = Get-Target
    Write-Info "Platform: $target"

    Write-Host ""
    Write-Host "  This will install DuDuClaw to: $InstallDir\$BinaryName"

    # Try release binary first
    $installed = Install-FromRelease -Target $target

    if (-not $installed) {
        Write-Warn "Pre-built binary download failed for $target (v$DuDuClawVersion)."
        Write-Host ""
        Write-Host "  Recommended: install the prebuilt binary via npm instead (no MSVC/compiler needed):" -ForegroundColor White
        Write-Host "       npm install -g duduclaw" -ForegroundColor Cyan
        Write-Host ""
        Write-Host "  Building from source requires Rust + MSVC Build Tools (~2GB) and can take ~1.5h." -ForegroundColor Yellow
        Write-Host ""
        $answer = Read-Host "  Build from source using cargo instead? [y/N]"
        if ($answer -match "^[Yy]") {
            Install-FromSource
        } else {
            throw "Installation cancelled. Use 'npm install -g duduclaw' for the prebuilt binary."
        }
    }

    # Add to PATH
    Add-ToPath

    # Check optional dependencies
    Test-Python
    Test-Docker
    Test-WSL

    # Success
    Write-Heading "Installation complete!"
    Write-Host ""
    Write-Host "  Next steps:" -ForegroundColor White
    Write-Host ""
    Write-Host "    1. Restart your terminal (or open a new one)"
    Write-Host ""
    Write-Host "    2. Run the onboarding wizard:"
    Write-Host "       duduclaw onboard" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "    3. Start the gateway:"
    Write-Host "       duduclaw gateway start" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  Documentation: https://github.com/$GitHubRepo"
    Write-Host ""
}

Main
