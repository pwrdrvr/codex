param(
  [Parameter(Mandatory = $true)]
  [string]$OutputRoot
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$trustedSigningVersion = "0.5.8"

$resolvedOutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)
$moduleRoot = Join-Path $resolvedOutputRoot "modules"
$localAppDataRoot = Join-Path $resolvedOutputRoot "localappdata"
New-Item -ItemType Directory -Force $moduleRoot, $localAppDataRoot | Out-Null

Save-Module `
  -Name TrustedSigning `
  -RequiredVersion $trustedSigningVersion `
  -Repository PSGallery `
  -Path $moduleRoot

$moduleManifest = Join-Path `
  $moduleRoot `
  "TrustedSigning/$trustedSigningVersion/TrustedSigning.psd1"
if (-not (Test-Path -LiteralPath $moduleManifest)) {
  throw "TrustedSigning $trustedSigningVersion was not saved to $moduleManifest."
}

$moduleMetadata = Import-PowerShellDataFile -LiteralPath $moduleManifest
if ([string]$moduleMetadata.ModuleVersion -ne $trustedSigningVersion) {
  throw "Expected TrustedSigning $trustedSigningVersion, got $($moduleMetadata.ModuleVersion)."
}
if ([string]$moduleMetadata.CompanyName -ne "Microsoft") {
  throw "Expected the TrustedSigning module publisher to be Microsoft."
}

$catalogPath = Join-Path (Split-Path $moduleManifest) "catalog.cat"
$moduleFiles = @(
  Get-ChildItem -LiteralPath (Split-Path $moduleManifest) -File -Recurse -Force |
    Where-Object {
      $_.Name -ne "PSGetModuleInfo.xml" -and
      $_.FullName -ne $catalogPath
    }
)
$duplicateLeafNames = @(
  $moduleFiles |
    Group-Object Name |
    Where-Object Count -gt 1 |
    Select-Object -ExpandProperty Name
)
if ($duplicateLeafNames.Count -ne 0) {
  throw "TrustedSigning module has duplicate catalog leaf names: $($duplicateLeafNames -join ', ')"
}
$catalogResult = Test-FileCatalog `
  -Detailed `
  -Path $moduleFiles.FullName `
  -CatalogFilePath $catalogPath
if ([string]$catalogResult.Status -ne "Valid") {
  $catalogKeys = @($catalogResult.CatalogItems.Keys)
  $pathKeys = @($catalogResult.PathItems.Keys)
  $mismatches = foreach ($key in @($catalogKeys + $pathKeys | Sort-Object -Unique)) {
    $catalogHash = [string]$catalogResult.CatalogItems[$key]
    $pathHash = [string]$catalogResult.PathItems[$key]
    if ($catalogHash -ne $pathHash) {
      "$key (catalog=$catalogHash, path=$pathHash)"
    }
  }
  throw "TrustedSigning module catalog validation failed: $($catalogResult.Status); $($mismatches -join '; ')"
}
if ($null -eq $catalogResult.Signature) {
  throw "TrustedSigning module catalog validation returned no signature."
}
if ($catalogResult.Signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
  throw "TrustedSigning module catalog signature is invalid: $($catalogResult.Signature.Status)"
}
if ($null -eq $catalogResult.Signature.SignerCertificate) {
  throw "TrustedSigning module catalog validation returned no signer certificate."
}
$catalogSigner = $catalogResult.Signature.SignerCertificate.GetNameInfo(
  [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName,
  $false
)
if ($catalogSigner -ne "Microsoft Corporation") {
  throw "Unexpected TrustedSigning module catalog signer: $catalogSigner"
}

$env:LOCALAPPDATA = $localAppDataRoot
Import-Module $moduleManifest -Force -ErrorAction Stop
$dependencyModule = Join-Path `
  (Split-Path $moduleManifest) `
  "NugetInstall/NugetInstall.psd1"
Import-Module $dependencyModule -Force -ErrorAction Stop
$dependencies = Get-EveryDependency

foreach ($dependencyPath in @(
  $dependencies.DlibFolderPath,
  $dependencies.SignToolFolderPath,
  $dependencies.SignCliFolderPath
)) {
  if (-not (Test-Path -LiteralPath $dependencyPath)) {
    throw "TrustedSigning dependency was not prepared: $dependencyPath"
  }
}

$checksumManifest = Join-Path $resolvedOutputRoot "SHA256SUMS"
$filesToChecksum = @(
  Get-ChildItem -LiteralPath $resolvedOutputRoot -File -Recurse -Force |
    Where-Object { $_.FullName -ne $checksumManifest } |
    Sort-Object FullName
)
$checksumLines = @(
  $filesToChecksum | ForEach-Object {
    $relativePath = [System.IO.Path]::GetRelativePath($resolvedOutputRoot, $_.FullName)
    $sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
    "$sha256  $($relativePath.Replace('\', '/'))"
  }
)
$checksumLines | Set-Content -LiteralPath $checksumManifest -Encoding ascii

Write-Host "Prepared pinned TrustedSigning $trustedSigningVersion and its dependencies in $resolvedOutputRoot."
