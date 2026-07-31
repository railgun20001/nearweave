param(
    [Parameter(Mandatory = $true)]
    [string]$Installer,

    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$Repository,

    [Parameter(Mandatory = $true)]
    [string]$Output,

    [string]$Notes = "NearWeave Windows x64 稳定版"
)

$ErrorActionPreference = "Stop"
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$signaturePath = "$installerPath.sig"
if (-not (Test-Path -LiteralPath $signaturePath -PathType Leaf)) {
    throw "Updater signature not found: $signaturePath"
}

$assetName = Split-Path -Leaf $installerPath
$manifest = [ordered]@{
    version = $Version
    notes = $Notes
    pub_date = [DateTimeOffset]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = (Get-Content -LiteralPath $signaturePath -Raw -Encoding UTF8).Trim()
            url = "https://github.com/$Repository/releases/download/v$Version/$assetName"
        }
    }
}

$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $Output -Encoding UTF8
