[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Owner,

    [Parameter(Mandatory = $true)]
    [string] $Repository,

    [string] $UpdaterRepository = "nearweave-updates",

    [string] $MainBranch = "main",

    [string] $ReleaseTag = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$giteeToken = $env:GITEE_TOKEN
if ([string]::IsNullOrWhiteSpace($giteeToken)) {
    throw "缺少 GITEE_TOKEN，无法同步 Gitee。"
}

if ($Owner -notmatch "^[A-Za-z0-9._-]+$") {
    throw "Gitee 仓库所有者格式不合法：$Owner"
}

if ($Repository -notmatch "^[A-Za-z0-9._-]+$") {
    throw "Gitee 仓库名称格式不合法：$Repository"
}

if ($UpdaterRepository -notmatch "^[A-Za-z0-9._-]+$") {
    throw "Gitee 更新清单仓库名称格式不合法：$UpdaterRepository"
}

if ($MainBranch -notmatch "^[A-Za-z0-9._/-]+$") {
    throw "Gitee 主分支格式不合法：$MainBranch"
}

if (
    -not [string]::IsNullOrWhiteSpace($ReleaseTag) -and
    $ReleaseTag -notmatch "^v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?$"
) {
    throw "发布标签格式不合法：$ReleaseTag"
}

$runnerTemp = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    [IO.Path]::GetTempPath()
} else {
    $env:RUNNER_TEMP
}
$runnerTemp = [IO.Path]::GetFullPath($runnerTemp)

$taskId = [Guid]::NewGuid().ToString("N")
$askPassPath = Join-Path $runnerTemp "nearweave-gitee-askpass-$taskId.cmd"
$assetsDirectory = Join-Path $runnerTemp "nearweave-gitee-assets-$taskId"
$updaterWorktree = Join-Path $runnerTemp "nearweave-gitee-updater-$taskId"

function Invoke-Git {
    param(
        [Parameter(Mandatory = $true)]
        [string[]] $Arguments,

        [string] $WorkingDirectory = $PWD.Path
    )

    & git -C $WorkingDirectory @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Git 命令执行失败：git $($Arguments -join ' ')"
    }
}

function Invoke-GiteeGit {
    param(
        [Parameter(Mandatory = $true)]
        [string[]] $Arguments,

        [string] $WorkingDirectory = $PWD.Path
    )

    # 禁用 runner 预装的凭据管理器，确保只通过本任务的 GIT_ASKPASS 提供令牌。
    & git -c credential.helper= -C $WorkingDirectory @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Gitee Git 命令执行失败：git $($Arguments -join ' ')"
    }
}

function Remove-TaskDirectory {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,

        [Parameter(Mandatory = $true)]
        [string] $ExpectedPrefix
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    $resolvedPath = [IO.Path]::GetFullPath($Path)
    $tempPrefix = $runnerTemp.TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    ) + [IO.Path]::DirectorySeparatorChar
    $leafName = [IO.Path]::GetFileName($resolvedPath)

    if (
        -not $resolvedPath.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -or
        -not $leafName.StartsWith($ExpectedPrefix, [StringComparison]::Ordinal)
    ) {
        throw "拒绝清理非本任务临时目录：$resolvedPath"
    }

    Remove-Item -LiteralPath $resolvedPath -Recurse -Force
}

function Get-GiteeRelease {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Tag
    )

    $encodedTag = [Uri]::EscapeDataString($Tag)
    $uri = "https://gitee.com/api/v5/repos/$Owner/$Repository/releases/tags/$encodedTag"

    try {
        return Invoke-RestMethod -Method Get -Uri $uri -TimeoutSec 30
    } catch {
        $statusCode = $_.Exception.Response.StatusCode.value__
        if ($statusCode -eq 404) {
            return $null
        }

        throw "读取 Gitee Release 失败：HTTP $statusCode"
    }
}

function New-GiteeRelease {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Tag
    )

    $releaseBody = @"
NearWeave $Tag Windows x64 安装包。
已安装 v0.4.0 及更高版本可在“设置 → 软件更新”中完成签名校验和自动升级。
源代码与发布记录由 GitHub Actions 单向同步，GitHub 仓库仍是唯一事实源。
"@

    $form = @{
        access_token     = $giteeToken
        tag_name         = $Tag
        target_commitish = $Tag
        name             = "NearWeave $Tag"
        body             = $releaseBody
        prerelease       = "false"
    }

    try {
        return Invoke-RestMethod `
            -Method Post `
            -Uri "https://gitee.com/api/v5/repos/$Owner/$Repository/releases" `
            -ContentType "application/x-www-form-urlencoded" `
            -Body $form `
            -TimeoutSec 60
    } catch {
        $message = $_.Exception.Message.Replace($giteeToken, "***")
        throw "创建 Gitee Release 失败：$message"
    }
}

function Remove-MatchingGiteeAttachments {
    param(
        [Parameter(Mandatory = $true)]
        [long] $ReleaseId,

        [Parameter(Mandatory = $true)]
        [string[]] $Names
    )

    $baseUri = "https://gitee.com/api/v5/repos/$Owner/$Repository/releases/$ReleaseId/attach_files"
    try {
        $response = Invoke-RestMethod -Method Get -Uri $baseUri -TimeoutSec 30
        $attachments = @($response) | Where-Object { $null -ne $_ }
    } catch {
        $statusCode = $_.Exception.Response.StatusCode.value__
        throw "读取 Gitee Release 附件失败：HTTP $statusCode"
    }

    foreach ($attachment in $attachments) {
        if ($Names -notcontains [string] $attachment.name) {
            continue
        }

        $encodedToken = [Uri]::EscapeDataString($giteeToken)
        $deleteUri = "$baseUri/$($attachment.id)?access_token=$encodedToken"
        try {
            Invoke-RestMethod -Method Delete -Uri $deleteUri -TimeoutSec 60 | Out-Null
        } catch {
            $message = $_.Exception.Message.Replace($giteeToken, "***")
            throw "删除 Gitee Release 旧附件失败：$message"
        }
    }
}

function Send-GiteeAttachment {
    param(
        [Parameter(Mandatory = $true)]
        [long] $ReleaseId,

        [Parameter(Mandatory = $true)]
        [IO.FileInfo] $File
    )

    $uri = "https://gitee.com/api/v5/repos/$Owner/$Repository/releases/$ReleaseId/attach_files"
    $curlFilePath = $File.FullName.Replace("\", "/")
    $curlConfig = @"
url = "$uri"
request = "POST"
form = "access_token=$giteeToken"
form = "file=@$curlFilePath"
connect-timeout = 30
max-time = 300
silent
show-error
fail-with-body
"@

    # 通过标准输入传入 curl 配置，避免把令牌放进进程命令行和 Actions 日志。
    $response = $curlConfig | & curl.exe --config -
    if ($LASTEXITCODE -ne 0) {
        $message = ([string] $response).Replace($giteeToken, "***")
        throw "上传 Gitee Release 附件 $($File.Name) 失败：$message"
    }
}

function Publish-UpdaterManifest {
    param(
        [Parameter(Mandatory = $true)]
        [IO.FileInfo] $Manifest,

        [Parameter(Mandatory = $true)]
        [string] $Tag
    )

    $updaterRemoteUrl = "https://$Owner@gitee.com/$Owner/$UpdaterRepository.git"
    & git -c credential.helper= ls-remote --exit-code --heads $updaterRemoteUrl main *> $null
    $updaterBranchExists = $LASTEXITCODE -eq 0
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 2) {
        throw "无法检查 Gitee 更新清单仓库 main 分支。"
    }

    if ($updaterBranchExists) {
        Invoke-GiteeGit -Arguments @(
            "clone",
            "--origin",
            "gitee-updater",
            "--branch",
            "main",
            "--single-branch",
            $updaterRemoteUrl,
            $updaterWorktree
        )
    } else {
        New-Item -ItemType Directory -Path $updaterWorktree | Out-Null
        Invoke-Git -WorkingDirectory $updaterWorktree -Arguments @("init", "--initial-branch=main")
        Invoke-Git -WorkingDirectory $updaterWorktree -Arguments @(
            "remote",
            "add",
            "gitee-updater",
            $updaterRemoteUrl
        )
    }

    Copy-Item -LiteralPath $Manifest.FullName -Destination (Join-Path $updaterWorktree "latest.json") -Force
    Invoke-Git -WorkingDirectory $updaterWorktree -Arguments @("config", "user.name", "github-actions[bot]")
    Invoke-Git -WorkingDirectory $updaterWorktree -Arguments @(
        "config",
        "user.email",
        "41898282+github-actions[bot]@users.noreply.github.com"
    )
    Invoke-Git -WorkingDirectory $updaterWorktree -Arguments @("add", "--", "latest.json")

    & git -C $updaterWorktree diff --cached --quiet
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Gitee 更新清单仓库 main/latest.json 已是 $Tag，无需重复提交。"
        return
    }
    if ($LASTEXITCODE -ne 1) {
        throw "无法检查更新清单 latest.json 差异。"
    }

    Invoke-Git -WorkingDirectory $updaterWorktree -Arguments @(
        "commit",
        "-m",
        "发布：更新 $Tag 国内更新清单"
    )
    Invoke-GiteeGit -WorkingDirectory $updaterWorktree -Arguments @(
        "push",
        "gitee-updater",
        "HEAD:refs/heads/main"
    )
}

try {
    # GIT_ASKPASS 文件只引用 Actions 注入的环境变量，不把令牌本身落盘。
    $askPassContent = @"
@echo off
echo %~1 | findstr /I "Username" >nul
if %errorlevel%==0 (
  echo $Owner
) else (
  echo %GITEE_TOKEN%
)
"@
    Set-Content -LiteralPath $askPassPath -Value $askPassContent -Encoding ascii
    $env:GIT_ASKPASS = $askPassPath
    $env:GIT_TERMINAL_PROMPT = "0"

    $giteeRemoteUrl = "https://$Owner@gitee.com/$Owner/$Repository.git"
    $remoteNames = @(& git remote)
    if ($remoteNames -contains "gitee") {
        Invoke-Git -Arguments @("remote", "set-url", "gitee", $giteeRemoteUrl)
    } else {
        Invoke-Git -Arguments @("remote", "add", "gitee", $giteeRemoteUrl)
    }

    Invoke-Git -Arguments @("fetch", "origin", $MainBranch, "--tags", "--force")
    Invoke-GiteeGit -Arguments @(
        "push",
        "gitee",
        "refs/remotes/origin/${MainBranch}:refs/heads/${MainBranch}"
    )
    Invoke-GiteeGit -Arguments @("push", "gitee", "--tags")
    Write-Host "Gitee $MainBranch 与标签已完成单向同步。"

    if ([string]::IsNullOrWhiteSpace($ReleaseTag)) {
        return
    }

    New-Item -ItemType Directory -Path $assetsDirectory | Out-Null
    & gh release download $ReleaseTag `
        --repo "$Owner/$Repository" `
        --pattern "*.exe" `
        --pattern "*.sig" `
        --pattern "latest.json" `
        --dir $assetsDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "下载 GitHub Release $ReleaseTag 的更新附件失败。"
    }

    $installerFiles = @(
        Get-ChildItem -LiteralPath $assetsDirectory -File |
            Where-Object { $_.Name.EndsWith(".exe", [StringComparison]::OrdinalIgnoreCase) }
    )
    if ($installerFiles.Count -ne 1) {
        throw "Gitee 发布当前要求恰好一个 NSIS .exe，实际找到 $($installerFiles.Count) 个。"
    }
    $installer = $installerFiles[0]

    $signaturePath = "$($installer.FullName).sig"
    if (-not (Test-Path -LiteralPath $signaturePath -PathType Leaf)) {
        throw "缺少安装包签名：$($installer.Name).sig"
    }
    $signature = Get-Item -LiteralPath $signaturePath

    $manifestPath = Join-Path $assetsDirectory "latest.json"
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "GitHub Release 缺少 latest.json。"
    }

    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ("v$($manifest.version)" -ne $ReleaseTag) {
        throw "latest.json 版本 v$($manifest.version) 与标签 $ReleaseTag 不一致。"
    }

    $platforms = @($manifest.platforms.PSObject.Properties)
    if ($platforms.Count -eq 0) {
        throw "latest.json 未包含任何更新平台。"
    }

    $encodedInstallerName = [Uri]::EscapeDataString($installer.Name)
    $giteeDownloadUrl = "https://gitee.com/$Owner/$Repository/releases/download/$ReleaseTag/$encodedInstallerName"
    foreach ($platform in $platforms) {
        if ([string]::IsNullOrWhiteSpace([string] $platform.Value.signature)) {
            throw "latest.json 平台 $($platform.Name) 缺少签名。"
        }
        $platform.Value.url = $giteeDownloadUrl
    }
    $manifest | ConvertTo-Json -Depth 20 |
        Set-Content -LiteralPath $manifestPath -Encoding utf8NoBOM
    $manifestFile = Get-Item -LiteralPath $manifestPath

    $giteeRelease = Get-GiteeRelease -Tag $ReleaseTag
    if ($null -eq $giteeRelease) {
        $giteeRelease = New-GiteeRelease -Tag $ReleaseTag
    }
    if ($null -eq $giteeRelease.id) {
        throw "Gitee Release 响应缺少 id。"
    }

    $attachmentNames = @($installer.Name, $signature.Name, $manifestFile.Name)
    Remove-MatchingGiteeAttachments `
        -ReleaseId ([long] $giteeRelease.id) `
        -Names $attachmentNames
    Send-GiteeAttachment -ReleaseId ([long] $giteeRelease.id) -File $installer
    Send-GiteeAttachment -ReleaseId ([long] $giteeRelease.id) -File $signature
    Send-GiteeAttachment -ReleaseId ([long] $giteeRelease.id) -File $manifestFile

    Publish-UpdaterManifest -Manifest $manifestFile -Tag $ReleaseTag
    Write-Host "Gitee Release $ReleaseTag 与国内更新清单发布完成。"
} finally {
    Remove-TaskDirectory -Path $updaterWorktree -ExpectedPrefix "nearweave-gitee-updater-"
    Remove-TaskDirectory -Path $assetsDirectory -ExpectedPrefix "nearweave-gitee-assets-"

    if (Test-Path -LiteralPath $askPassPath) {
        $resolvedAskPass = [IO.Path]::GetFullPath($askPassPath)
        $tempPrefix = $runnerTemp.TrimEnd(
            [IO.Path]::DirectorySeparatorChar,
            [IO.Path]::AltDirectorySeparatorChar
        ) + [IO.Path]::DirectorySeparatorChar
        if (
            $resolvedAskPass.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedAskPass).StartsWith(
                "nearweave-gitee-askpass-",
                [StringComparison]::Ordinal
            )
        ) {
            Remove-Item -LiteralPath $resolvedAskPass -Force
        } else {
            Write-Warning "拒绝清理非本任务 GIT_ASKPASS 文件：$resolvedAskPass"
        }
    }
}
