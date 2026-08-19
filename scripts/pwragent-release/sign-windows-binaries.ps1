param(
  [Parameter(Mandatory = $true)]
  [string[]]$BinaryPath,

  [Parameter(Mandatory = $true)]
  [string]$SigningToolsRoot
)

$ErrorActionPreference = "Stop"
$expectedPublisher = "PwrDrvr LLC"

$requiredEnvironment = [ordered]@{
  WIN_AZURE_SIGN_PUBLISHER_NAME = $env:WIN_AZURE_SIGN_PUBLISHER_NAME
  WIN_AZURE_SIGN_ENDPOINT = $env:WIN_AZURE_SIGN_ENDPOINT
  WIN_AZURE_SIGN_ACCOUNT = $env:WIN_AZURE_SIGN_ACCOUNT
  WIN_AZURE_SIGN_PROFILE = $env:WIN_AZURE_SIGN_PROFILE
  AZURE_TENANT_ID = $env:AZURE_TENANT_ID
  AZURE_CLIENT_ID = $env:AZURE_CLIENT_ID
  AZURE_CLIENT_SECRET = $env:AZURE_CLIENT_SECRET
}
$missing = @(
  $requiredEnvironment.GetEnumerator() |
    Where-Object { [string]::IsNullOrWhiteSpace([string]$_.Value) } |
    ForEach-Object Key
)
if ($missing.Count -gt 0) {
  throw "Windows release signing is required, but configuration is missing: $($missing -join ', ')"
}
if ($env:WIN_AZURE_SIGN_PUBLISHER_NAME -ne $expectedPublisher) {
  throw "WIN_AZURE_SIGN_PUBLISHER_NAME must be '$expectedPublisher'."
}

# Resolve every path before signing anything, so a typo fails the job before it
# spends a signing operation rather than halfway through the binary set.
$resolvedBinaries = @(
  $BinaryPath | ForEach-Object {
    $resolved = Resolve-Path -LiteralPath $_ -ErrorAction Stop
    if ($resolved.Count -ne 1) {
      throw "Expected exactly one path for '$_', got $($resolved.Count)."
    }
    $resolved.Path
  }
)
if ($resolvedBinaries.Count -eq 0) {
  throw "No binaries were supplied to sign."
}

$verifiedSigningTools = & (Join-Path $PSScriptRoot "verify-trusted-signing-tools.ps1") `
  -SigningToolsRoot $SigningToolsRoot
$moduleManifest = $verifiedSigningTools.ModuleManifest
$env:LOCALAPPDATA = $verifiedSigningTools.LocalAppDataRoot

Import-Module $moduleManifest -Force -ErrorAction Stop

# One call covering every file. Invoke-TrustedSigning accepts a file list, and a
# single call keeps the signing account round-trips proportional to releases
# rather than to the number of binaries Codex ships.
$signingParameters = @{
  Endpoint = $env:WIN_AZURE_SIGN_ENDPOINT
  CodeSigningAccountName = $env:WIN_AZURE_SIGN_ACCOUNT
  CertificateProfileName = $env:WIN_AZURE_SIGN_PROFILE
  Files = $resolvedBinaries
  FileDigest = "SHA256"
  TimestampRfc3161 = "http://timestamp.acs.microsoft.com"
  TimestampDigest = "SHA256"
}
Invoke-TrustedSigning @signingParameters

$expectedCommonName = "CN=$expectedPublisher"
foreach ($resolvedBinary in $resolvedBinaries) {
  $signature = Get-AuthenticodeSignature -LiteralPath $resolvedBinary
  if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "Authenticode verification failed for ${resolvedBinary}: $($signature.Status) ($($signature.StatusMessage))"
  }
  if ($null -eq $signature.SignerCertificate) {
    throw "Authenticode verification returned no signer certificate for $resolvedBinary."
  }
  if (-not $signature.SignerCertificate.Subject.StartsWith("$expectedCommonName,")) {
    throw "Unexpected Authenticode signer for ${resolvedBinary}: $($signature.SignerCertificate.Subject)"
  }
  if ($null -eq $signature.TimeStamperCertificate) {
    throw "The Authenticode signature for $resolvedBinary is valid but is not timestamped."
  }

  Write-Host "Verified $resolvedBinary"
  Write-Host "  Authenticode signer: $($signature.SignerCertificate.Subject)"
  Write-Host "  RFC 3161 timestamp certificate: $($signature.TimeStamperCertificate.Subject)"
}
