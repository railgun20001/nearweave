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
        throw "检测到多个旧版当前用户安装项，已中止安装"
    }

    if ($candidates.Count -eq 0) {
        if (Test-Path -LiteralPath $expectedOldProgram -PathType Leaf) {
            throw "检测到没有有效卸载项的旧版程序，已中止安装"
        }
        $state | ConvertTo-Json | Set-Content -LiteralPath $StateFile -Encoding UTF8
        exit 0
    }

    $entry = $candidates[0]
    if ([string]$entry.Publisher -cne $publisher) {
        throw "旧版安装项发布者不匹配，已中止安装"
    }
    $oldVersion = $null
    if (-not [version]::TryParse([string]$entry.DisplayVersion, [ref]$oldVersion) -or
        $oldVersion -lt [version]"0.1.0" -or $oldVersion -gt [version]"0.3.1") {
        throw "旧版安装项版本不在允许迁移的范围内，已中止安装"
    }
    if ((Normalize-Path ([string]$entry.InstallLocation)) -ine $expectedOldInstall) {
        throw "旧版安装路径异常，已中止安装"
    }
    if ((Get-CommandPath ([string]$entry.UninstallString)) -ine $expectedOldUninstaller) {
        throw "旧版卸载程序路径异常，已中止安装"
    }
    if (-not (Test-Path -LiteralPath $expectedOldUninstaller -PathType Leaf) -or
        -not (Test-Path -LiteralPath $expectedOldProgram -PathType Leaf)) {
        throw "旧版安装文件不完整，已中止安装"
    }

    $running = @(Get-CimInstance Win32_Process -Filter "Name = '$oldBinaryName'" | Where-Object {
        $_.ExecutablePath -and (Normalize-Path $_.ExecutablePath) -ieq $expectedOldProgram
    })
    if ($running.Count -gt 0) {
        throw "旧版程序仍在运行，请退出后重试"
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
        throw "旧版卸载程序返回错误码 $($process.ExitCode)，已中止安装"
    }

    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        if (-not (Test-Path -LiteralPath $expectedOldProgram) -and
            -not (Test-Path -LiteralPath $entry.PSPath)) {
            break
        }
        Start-Sleep -Milliseconds 250
    }
    if ((Test-Path -LiteralPath $expectedOldProgram) -or (Test-Path -LiteralPath $entry.PSPath)) {
        throw "旧版卸载未完整结束，已中止安装"
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
    throw "安装迁移状态文件不存在"
}
$state = Get-Content -LiteralPath $StateFile -Raw -Encoding UTF8 | ConvertFrom-Json
try {
    if ($state.restoreAutostart -eq $true) {
        $expectedNewProgram = Normalize-Path (Join-Path $env:LOCALAPPDATA "NearWeave\nearweave.exe")
        $normalizedProgram = Normalize-Path $Program
        if ($normalizedProgram -ine $expectedNewProgram -or
            -not (Test-Path -LiteralPath $normalizedProgram -PathType Leaf)) {
            throw "NearWeave 安装路径异常，无法恢复开机启动"
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
