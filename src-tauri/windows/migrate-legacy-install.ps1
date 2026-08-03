param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("PreInstall", "PostInstall")]
    [string]$Phase,

    [Parameter(Mandatory = $true)]
    [string]$StateFile,

    [string]$Program
)

$ErrorActionPreference = "Stop"

function Decode-Unicode([string]$Value) {
    return [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($Value))
}

function Normalize-Path([string]$Value) {
    return [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($Value)).TrimEnd('\')
}

function Get-CommandPath([string]$Command) {
    if ([string]::IsNullOrWhiteSpace($Command)) {
        return $null
    }
    $match = [regex]::Match($Command.Trim(), '^(?:"(?<quoted>[^"]+)"|(?<plain>[^\s]+))')
    if (-not $match.Success) {
        return $null
    }
    $value = if ($match.Groups['quoted'].Success) {
        $match.Groups['quoted'].Value
    } else {
        $match.Groups['plain'].Value
    }
    return Normalize-Path $value
}

function Remove-VerifiedShortcut([string]$ShortcutPath, [string]$ExpectedTarget) {
    if (-not (Test-Path -LiteralPath $ShortcutPath -PathType Leaf)) {
        return
    }
    $shell = New-Object -ComObject WScript.Shell
    $target = Normalize-Path $shell.CreateShortcut($ShortcutPath).TargetPath
    if ($target -ieq $ExpectedTarget) {
        Remove-Item -LiteralPath $ShortcutPath -Force
    }
}

$oldProductName = Decode-Unicode "3YRlaA=="
$oldBinaryName = Decode-Unicode "YgBsAHUAZQB0AG8AbwB0AGgALQBzAGgAYQByAGUALgBlAHgAZQA="
$publisher = "railgun20001"
$uninstallRoot = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall"
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$expectedOldInstall = Normalize-Path (Join-Path $env:LOCALAPPDATA $oldProductName)
$expectedOldProgram = Normalize-Path (Join-Path $expectedOldInstall $oldBinaryName)
$expectedOldUninstaller = Normalize-Path (Join-Path $expectedOldInstall "uninstall.exe")

if ($Phase -eq "PreInstall") {
    $state = [ordered]@{
        found = $false
        restoreAutostart = $false
    }

    $candidates = @()
    if (Test-Path -LiteralPath $uninstallRoot) {
        $candidates = @(Get-ChildItem -LiteralPath $uninstallRoot | ForEach-Object {
            Get-ItemProperty -LiteralPath $_.PSPath
        } | Where-Object { $_.DisplayName -ceq $oldProductName })
    }

    if ($candidates.Count -gt 1) {
        throw "Multiple legacy per-user installation entries were found"
    }

    if ($candidates.Count -eq 0) {
        if (Test-Path -LiteralPath $expectedOldProgram -PathType Leaf) {
            throw "The legacy program exists without a valid uninstall entry"
        }
        $state | ConvertTo-Json | Set-Content -LiteralPath $StateFile -Encoding UTF8
        exit 0
    }

    $entry = $candidates[0]
    if ([string]$entry.Publisher -cne $publisher) {
        throw "The legacy installation publisher does not match"
    }
    $oldVersion = $null
    if (-not [version]::TryParse([string]$entry.DisplayVersion, [ref]$oldVersion) -or
        $oldVersion -lt [version]"0.1.0" -or $oldVersion -gt [version]"0.3.1") {
        throw "The legacy version is outside the supported migration range"
    }
    if ((Normalize-Path ([string]$entry.InstallLocation)) -ine $expectedOldInstall) {
        throw "The legacy installation path does not match"
    }
    if ((Get-CommandPath ([string]$entry.UninstallString)) -ine $expectedOldUninstaller) {
        throw "The legacy uninstaller path does not match"
    }
    if (-not (Test-Path -LiteralPath $expectedOldUninstaller -PathType Leaf) -or
        -not (Test-Path -LiteralPath $expectedOldProgram -PathType Leaf)) {
        throw "The legacy installation files are incomplete"
    }

    $running = @(Get-CimInstance Win32_Process -Filter "Name = '$oldBinaryName'" | Where-Object {
        $_.ExecutablePath -and (Normalize-Path $_.ExecutablePath) -ieq $expectedOldProgram
    })
    if ($running.Count -gt 0) {
        throw "The legacy program is still running"
    }

    if (Test-Path -LiteralPath $runKey) {
        $runEntry = Get-ItemProperty -LiteralPath $runKey -Name $oldProductName -ErrorAction SilentlyContinue
        if ($runEntry) {
            $runCommand = [string]$runEntry.$oldProductName
            if ((Get-CommandPath $runCommand) -ieq $expectedOldProgram) {
                $state.restoreAutostart = $true
            }
        }
    }

    $state.found = $true
    $state | ConvertTo-Json | Set-Content -LiteralPath $StateFile -Encoding UTF8

    $process = Start-Process -FilePath $expectedOldUninstaller -ArgumentList "/S" -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "The legacy uninstaller returned exit code $($process.ExitCode)"
    }

    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        if (-not (Test-Path -LiteralPath $expectedOldProgram) -and
            -not (Test-Path -LiteralPath $entry.PSPath)) {
            break
        }
        Start-Sleep -Milliseconds 250
    }
    if ((Test-Path -LiteralPath $expectedOldProgram) -or (Test-Path -LiteralPath $entry.PSPath)) {
        throw "The legacy uninstall did not complete"
    }

    $shortcutName = "$oldProductName.lnk"
    Remove-VerifiedShortcut `
        (Join-Path $env:USERPROFILE "Desktop\$shortcutName") `
        $expectedOldProgram
    Remove-VerifiedShortcut `
        (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$shortcutName") `
        $expectedOldProgram
    exit 0
}

if (-not (Test-Path -LiteralPath $StateFile -PathType Leaf)) {
    throw "The installation migration state file does not exist"
}
$state = Get-Content -LiteralPath $StateFile -Raw -Encoding UTF8 | ConvertFrom-Json
try {
    if ($state.restoreAutostart -eq $true) {
        $expectedNewProgram = Normalize-Path (Join-Path $env:LOCALAPPDATA "NearWeave\nearweave.exe")
        $normalizedProgram = Normalize-Path $Program
        if ($normalizedProgram -ine $expectedNewProgram -or
            -not (Test-Path -LiteralPath $normalizedProgram -PathType Leaf)) {
            throw "The NearWeave installation path is invalid for restoring autostart"
        }
        if (-not (Test-Path -LiteralPath $runKey)) {
            New-Item -Path $runKey -Force | Out-Null
        }
        New-ItemProperty `
            -LiteralPath $runKey `
            -Name "NearWeave" `
            -Value ('"{0}"' -f $normalizedProgram) `
            -PropertyType String `
            -Force | Out-Null
    }
} finally {
    Remove-Item -LiteralPath $StateFile -Force -ErrorAction SilentlyContinue
}
