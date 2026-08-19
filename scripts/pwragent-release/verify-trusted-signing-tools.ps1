param(
  [Parameter(Mandatory = $true)]
  [string]$SigningToolsRoot
)

$ErrorActionPreference = "Stop"
$trustedSigningVersion = "0.5.8"

$resolvedSigningToolsRoot = (Resolve-Path -LiteralPath $SigningToolsRoot).Path
$moduleManifest = Join-Path `
  $resolvedSigningToolsRoot `
  "modules/TrustedSigning/$trustedSigningVersion/TrustedSigning.psd1"
$localAppDataRoot = Join-Path $resolvedSigningToolsRoot "localappdata"
$checksumManifest = Join-Path $resolvedSigningToolsRoot "SHA256SUMS"
if (-not (Test-Path -LiteralPath $moduleManifest -PathType Leaf)) {
  throw "Pinned TrustedSigning module is missing: $moduleManifest"
}
if (-not (Test-Path -LiteralPath $checksumManifest -PathType Leaf)) {
  throw "Pinned TrustedSigning checksum manifest is missing."
}

$rootPrefix = $resolvedSigningToolsRoot.TrimEnd(
  [System.IO.Path]::DirectorySeparatorChar,
  [System.IO.Path]::AltDirectorySeparatorChar
) + [System.IO.Path]::DirectorySeparatorChar
$verifiedPaths = [System.Collections.Generic.HashSet[string]]::new(
  [System.StringComparer]::OrdinalIgnoreCase
)
foreach ($line in Get-Content -LiteralPath $checksumManifest) {
  if ($line -notmatch '^([a-f0-9]{64})  (.+)$') {
    throw "Malformed TrustedSigning checksum entry: $line"
  }
  $expectedSha256 = $Matches[1]
  $relativePath = $Matches[2].Replace('/', [System.IO.Path]::DirectorySeparatorChar)
  $fullPath = [System.IO.Path]::GetFullPath(
    (Join-Path $resolvedSigningToolsRoot $relativePath)
  )
  if (-not $fullPath.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "TrustedSigning checksum path escapes the prepared root: $relativePath"
  }
  if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
    throw "Prepared TrustedSigning file is missing: $relativePath"
  }
  $actualSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $fullPath).Hash.ToLowerInvariant()
  if ($actualSha256 -ne $expectedSha256) {
    throw "Prepared TrustedSigning checksum mismatch for $relativePath."
  }
  if (-not $verifiedPaths.Add($fullPath)) {
    throw "Duplicate TrustedSigning checksum entry: $relativePath"
  }
}

$preparedFiles = @(
  Get-ChildItem -LiteralPath $resolvedSigningToolsRoot -File -Recurse -Force |
    Where-Object { $_.FullName -ne $checksumManifest }
)
$uncoveredFiles = @(
  $preparedFiles |
    Where-Object { -not $verifiedPaths.Contains($_.FullName) } |
    ForEach-Object {
      [System.IO.Path]::GetRelativePath($resolvedSigningToolsRoot, $_.FullName).Replace('\', '/')
    }
)
if ($uncoveredFiles.Count -ne 0) {
  throw "TrustedSigning input files are not covered by SHA256SUMS: $($uncoveredFiles -join ', ')"
}
if ($preparedFiles.Count -ne $verifiedPaths.Count) {
  throw "TrustedSigning input file count does not match SHA256SUMS."
}

foreach ($dependencyRoot in @(
  "Microsoft.Windows.SDK.BuildTools/Microsoft.Windows.SDK.BuildTools.10.0.26100.4188",
  "Microsoft.Trusted.Signing.Client/Microsoft.Trusted.Signing.Client.1.0.95",
  "sign/sign.0.9.1-beta.24469.1"
)) {
  $dependencyPath = Join-Path $localAppDataRoot "TrustedSigning/$dependencyRoot"
  if (-not (Test-Path -LiteralPath $dependencyPath -PathType Container)) {
    throw "Pinned TrustedSigning dependency is missing: $dependencyPath"
  }
}

[pscustomobject]@{
  ModuleManifest = $moduleManifest
  LocalAppDataRoot = $localAppDataRoot
}
