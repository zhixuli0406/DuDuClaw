# Authenticode sign the DuDuClaw Windows installer (TODO §D4.2).
#
# ⚠️ FALLBACK PATH ONLY (see docs/guides/desktop-unblock.md 關卡 C 路徑 3).
# Since 2023-06 the CA/B Forum requires even OV code-signing keys to live on FIPS
# hardware (USB token / cloud HSM), so you can no longer buy a plain .pfx for CI.
# Prefer cloud signing — Azure Trusted Signing (路徑 1) or Certum SimplySign
# (路徑 2). This script remains for legacy .pfx stock or an HSM-exported temp cert.
#
# Requires (inject via CI secrets, never commit):
#   WINDOWS_CERT_PFX_BASE64   base64 of the .pfx (OV/EV) signing certificate
#   WINDOWS_CERT_PASSWORD     password for the .pfx
#
# Usage: pwsh scripts/desktop/sign-windows.ps1 -Artifact path\to\DuDuClaw_x64.msi
param(
    [Parameter(Mandatory = $true)][string]$Artifact
)
$ErrorActionPreference = "Stop"

if (-not $env:WINDOWS_CERT_PFX_BASE64) { throw "WINDOWS_CERT_PFX_BASE64 is required" }
if (-not $env:WINDOWS_CERT_PASSWORD)   { throw "WINDOWS_CERT_PASSWORD is required" }

$pfx = Join-Path $env:RUNNER_TEMP "duduclaw-cert.pfx"
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64))

try {
    & signtool sign `
        /f $pfx `
        /p $env:WINDOWS_CERT_PASSWORD `
        /fd SHA256 `
        /tr http://timestamp.digicert.com `
        /td SHA256 `
        $Artifact
    & signtool verify /pa /v $Artifact
}
finally {
    Remove-Item $pfx -Force -ErrorAction SilentlyContinue
}
Write-Host "Signed: $Artifact"
