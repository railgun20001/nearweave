param(
    [switch]$AllowUnsigned
)

$ErrorActionPreference = "Stop"

$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
$tauriConfigPath = Join-Path $projectRoot "src-tauri\tauri.conf.json"
$cargoManifestPath = Join-Path $projectRoot "src-tauri\Cargo.toml"
$packageManifestPath = Join-Path $projectRoot "package.json"

$tauriConfig = Get-Content -LiteralPath $tauriConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
$packageManifest = Get-Content -LiteralPath $packageManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$cargoManifest = Get-Content -LiteralPath $cargoManifestPath -Raw -Encoding UTF8
$cargoVersionMatch = [regex]::Match(
    $cargoManifest,
    '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"'
)

if (-not $cargoVersionMatch.Success) {
    throw "Unable to read the Cargo package version"
}

$versions = @(
    [string]$tauriConfig.version,
    [string]$packageManifest.version,
    [string]$cargoVersionMatch.Groups[1].Value
)
if (($versions | Sort-Object -Unique).Count -ne 1 -or $versions[0] -ne "0.4.0") {
    throw "Manifest versions must all be 0.4.0: $($versions -join ', ')"
}
if ($tauriConfig.productName -cne "NearWeave") {
    throw "The product name must be NearWeave"
}
if ($tauriConfig.identifier -cne "io.github.railgun20001.nearweave") {
    throw "The application identifier must be the NearWeave identifier"
}
if ($tauriConfig.bundle.publisher -cne "railgun20001") {
    throw "The Windows publisher metadata must match the GitHub maintainer"
}
if ($tauriConfig.bundle.license -cne "Apache-2.0") {
    throw "The bundle license must be Apache-2.0"
}
if ($tauriConfig.bundle.windows.nsis.installMode -cne "currentUser") {
    throw "NSIS installMode must remain currentUser"
}
if ($tauriConfig.bundle.windows.allowDowngrades -ne $false) {
    throw "The installer must reject downgrades"
}
if ($tauriConfig.bundle.targets -notcontains "nsis") {
    throw "Bundle targets must include NSIS"
}
if ($tauriConfig.bundle.createUpdaterArtifacts -ne $true) {
    throw "Tauri updater artifacts must be enabled"
}
if ([string]::IsNullOrWhiteSpace([string]$tauriConfig.plugins.updater.pubkey)) {
    throw "The preserved Tauri updater public key must not be empty"
}

$expectedEndpoints = @(
    "https://gitee.com/railgun20001/nearweave-updates/raw/main/latest.json",
    "https://github.com/railgun20001/nearweave/releases/latest/download/latest.json"
)
if ($tauriConfig.plugins.updater.endpoints.Count -ne 2 -or
    $tauriConfig.plugins.updater.endpoints[0] -cne $expectedEndpoints[0] -or
    $tauriConfig.plugins.updater.endpoints[1] -cne $expectedEndpoints[1]) {
    throw "Updater endpoints must use Gitee first and GitHub second"
}
if ($tauriConfig.plugins.updater.windows.installMode -cne "passive") {
    throw "Windows updater install mode must be passive"
}

$expectedInstallerHooks = "windows/installer-hooks.nsh"
if ($tauriConfig.bundle.windows.nsis.installerHooks -cne $expectedInstallerHooks) {
    throw "NSIS must load the NearWeave installer hooks"
}
$installerHooksPath = Join-Path $projectRoot "src-tauri\windows\installer-hooks.nsh"
$installerHelperPath = Join-Path $projectRoot "src-tauri\src\installer_helper.rs"
$mainSourcePath = Join-Path $projectRoot "src-tauri\src\main.rs"
$installerHooks = Get-Content -LiteralPath $installerHooksPath -Raw -Encoding UTF8
$installerHelper = Get-Content -LiteralPath $installerHelperPath -Raw -Encoding UTF8
$mainSource = Get-Content -LiteralPath $mainSourcePath -Raw -Encoding UTF8
foreach ($marker in @(
    "NSIS_HOOK_PREINSTALL",
    "NSIS_HOOK_POSTINSTALL",
    "NSIS_HOOK_PREUNINSTALL",
    "nearweave-installer-helper.exe",
    "--nearweave-installer-helper migrate-pre",
    "--nearweave-installer-helper migrate-post",
    '--nearweave-installer-helper firewall-${ACTION}',
    'NEARWEAVE_RUN_FIREWALL "add"',
    'NEARWEAVE_RUN_FIREWALL "remove"'
)) {
    if (-not $installerHooks.Contains($marker)) {
        throw "Installer hook is missing marker: $marker"
    }
}
foreach ($marker in @(
    "UDP 37991",
    "Dynamic TCP",
    "NET_FW_PROFILE2_ALL",
    'BSTR::from("LocalSubnet")',
    "SetEdgeTraversal(VARIANT_FALSE)",
    "SetApplicationName",
    "legacy_version_is_supported",
    "restore_autostart",
    "expected_installed_program",
    "CoCreateInstance(&NetFwPolicy2"
)) {
    if (-not $installerHelper.Contains($marker)) {
        throw "Native installer helper is missing marker: $marker"
    }
}
foreach ($marker in @(
    'windows_subsystem = "windows"',
    "run_installer_helper_from_args"
)) {
    if (-not $mainSource.Contains($marker)) {
        throw "Windows GUI helper entry point is missing marker: $marker"
    }
}
if ($installerHooks.Contains("powershell.exe") -or $installerHooks.Contains(".ps1")) {
    throw "Installer hooks must not launch PowerShell scripts"
}

$tokens = $null
$errors = $null
[Management.Automation.Language.Parser]::ParseFile(
    $MyInvocation.MyCommand.Path,
    [ref]$tokens,
    [ref]$errors
) | Out-Null
if ($errors.Count -gt 0) {
    throw "PowerShell syntax error in $($MyInvocation.MyCommand.Path)`: $($errors[0].Message)"
}

$generatedRoot = Join-Path $projectRoot "src-tauri\target\release\nsis"
$installerScript = Get-ChildItem -LiteralPath $generatedRoot -Recurse -Filter "installer.nsi" |
    Select-Object -First 1
if (-not $installerScript) {
    throw "Generated installer.nsi was not found; run pnpm tauri build first"
}

$scriptContent = Get-Content -LiteralPath $installerScript.FullName -Raw -Encoding UTF8
foreach ($marker in @(
    "Page custom PageReinstall PageLeaveReinstall",
    'nsis_tauri_utils::SemverCompare "${VERSION}"',
    "NSIS_HOOK_PREINSTALL",
    "installer-hooks.nsh"
)) {
    if (-not $scriptContent.Contains($marker)) {
        throw "Generated NSIS script is missing upgrade marker: $marker"
    }
}

$binaryPath = Join-Path $projectRoot "src-tauri\target\release\nearweave.exe"
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "Release executable was not found"
}
$versionInfo = (Get-Item -LiteralPath $binaryPath).VersionInfo
if (-not $versionInfo.ProductVersion.StartsWith($versions[0], [StringComparison]::Ordinal)) {
    throw "Executable product version does not match manifest version"
}
if (-not $versionInfo.FileVersion.StartsWith($versions[0], [StringComparison]::Ordinal)) {
    throw "Executable file version does not match manifest version"
}
if ($versionInfo.ProductName -cne "NearWeave" -or $versionInfo.CompanyName -cne "railgun20001") {
    throw "Executable ProductName or CompanyName metadata is incorrect"
}

$bundleRoot = Join-Path $projectRoot "src-tauri\target\release\bundle\nsis"
$installerName = "NearWeave_$($versions[0])_x64-setup.exe"
$installerPath = Join-Path $bundleRoot $installerName
if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
    throw "Expected installer was not found: $installerName"
}
$installerVersionInfo = (Get-Item -LiteralPath $installerPath).VersionInfo
if ($installerVersionInfo.ProductName -cne "NearWeave" -or
    $installerVersionInfo.CompanyName -cne "railgun20001" -or
    -not $installerVersionInfo.ProductVersion.StartsWith($versions[0], [StringComparison]::Ordinal) -or
    -not $installerVersionInfo.FileVersion.StartsWith($versions[0], [StringComparison]::Ordinal)) {
    throw "Installer ProductName, CompanyName or version metadata is incorrect"
}
$releaseWorkflow = Get-Content -LiteralPath (Join-Path $projectRoot ".github\workflows\release.yml") -Raw -Encoding UTF8
if (-not $releaseWorkflow.Contains('releaseAssetNamePattern: "nearweave_[version]_[arch][setup][ext]"')) {
    throw "Published release asset naming pattern is incorrect"
}
$updaterSignature = "$installerPath.sig"
if (-not (Test-Path -LiteralPath $updaterSignature -PathType Leaf)) {
    if (-not $AllowUnsigned) {
        throw "Tauri updater signature was not found for $installerName"
    }
    Write-Warning "Unsigned local validation enabled; updater signature validation was skipped."
}

$authenticode = Get-AuthenticodeSignature -LiteralPath $installerPath
if ($authenticode.Status -ne "Valid") {
    if (-not $AllowUnsigned) {
        throw "Installer Authenticode signature is not valid: $($authenticode.Status)"
    }
    Write-Warning "Unsigned v0.4.0 baseline validation enabled; Authenticode validation was skipped."
} elseif ($authenticode.SignerCertificate.Subject -notmatch "SignPath Foundation") {
    throw "Unexpected Authenticode publisher: $($authenticode.SignerCertificate.Subject)"
}

Write-Output "NearWeave installer migration validation passed for version $($versions[0])."
