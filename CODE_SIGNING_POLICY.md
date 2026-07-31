# Code signing policy

Free code signing provided by SignPath.io, certificate by SignPath Foundation.

## 签名范围

NearWeave 的 Windows 安装程序和应用可执行文件只允许由本仓库公开源代码及仓库内的 GitHub Actions 发布工作流构建。用于正式发布的签名请求必须关联到公开的 Git 提交和 GitHub 托管运行器生成的构建产物，不接受本机任意二进制上传签名。

Windows Authenticode 签名与 Tauri Updater 签名用途不同：

- Authenticode 由 SignPath.io 完成，证书发布者为 `SignPath Foundation`，用于验证 Windows 可执行文件和安装程序的发布来源。
- Tauri Updater 使用本项目独立保管的 minisign 私钥，用于应用内更新包校验；该密钥不作为 Windows 发布者身份证书。

每次签名发布都需要人工审批。维护者必须为 GitHub 和 SignPath 账号启用多因素身份验证，不得绕过 SignPath 的来源验证、受信任构建系统或签名审批策略。

## 团队角色

- Authors / Committers：[railgun20001](https://github.com/railgun20001)，负责维护源代码、构建脚本和仓库配置。
- Reviewers：[railgun20001](https://github.com/railgun20001)，负责审查非维护者提交的 Pull Request，包括发布工作流和构建脚本变更。
- Approvers：[railgun20001](https://github.com/railgun20001)，负责核对版本、来源提交、构建结果和发布内容后批准签名请求。

当团队成员发生变化时，本文件必须在下一次签名申请前同步更新。

## 发布和审计

- 正式版本使用与应用版本一致的 `v<版本号>` Git 标签。
- GitHub Actions 必须先完成构建和自动化检查，再把 GitHub 工作流产物提交给 SignPath。
- 签名完成后只发布 SignPath 返回的产物，并保留 GitHub Actions 与 SignPath 的来源记录。
- 签名产物中的 `ProductName` 必须为 `NearWeave`，`ProductVersion` 与 `FileVersion` 必须与源码清单和 Git 标签一致，`CompanyName` 必须为 `railgun20001`；SignPath Artifact Configuration 必须强制校验这些字段。
- 主程序和 NSIS 安装包分别提交一次来自 GitHub Artifact 的 SignPath 请求，并由 Approver 人工批准；安装包只能包含第一次请求返回的已签名主程序。
- 最终 Authenticode 安装包返回后，才允许生成 Tauri Updater `.sig`、`latest.json` 和 SHA-256 校验文件并发布。
- 发现私钥、签名令牌、构建来源或发布内容异常时，立即停止发布并撤销相关凭据或签名请求。

隐私行为见 [隐私政策](PRIVACY.md)，许可条款见 [Apache License 2.0](LICENSE)。
