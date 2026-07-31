# SignPath 免费开源签名与发布

## 当前状态

NearWeave v0.4.0 是 SignPath Foundation 申请所需的公开、可下载基线。该版本使用既有 Tauri Updater 密钥签名更新包，但在申请获批前不包含 Windows Authenticode。Release 必须明确标注申请仍在审核中。

项目使用 Apache-2.0，公开提供隐私政策、卸载能力、系统修改提示和 [Code signing policy](../CODE_SIGNING_POLICY.md)。维护者 `railgun20001` 同时承担 Author / Committer、Reviewer 与 Approver 角色，并为 GitHub 和 SignPath 启用多因素身份验证。

## 申请资料

- 项目名：`NearWeave`
- 仓库：`https://github.com/railgun20001/nearweave`
- 下载页：`https://github.com/railgun20001/nearweave/releases`
- 许可证：Apache-2.0
- 隐私政策：`https://github.com/railgun20001/nearweave/blob/main/PRIVACY.md`
- Code signing policy：`https://github.com/railgun20001/nearweave/blob/main/CODE_SIGNING_POLICY.md`
- Windows 安装包：`nearweave_0.4.0_x64-setup.exe`
- 预期 Authenticode 发布者：`SignPath Foundation`

申请表的联系人、2FA 与协议授权沿用维护者已经确认的信息。验证码必须由维护者本人完成；在验证码完成前不得声称申请已提交或获批。

## 获批后的 v0.4.1 流水线

`.github/workflows/release-signpath.yml` 只接受与清单版本一致的已存在标签，并要求配置 `SIGNPATH_API_TOKEN` 以及 SignPath 组织、项目、策略和两个 Artifact Configuration slug：

1. GitHub 托管 Windows Runner 完成测试并构建 `nearweave.exe`。
2. 先上传 GitHub Artifact，再提交第一次 SignPath 请求，等待 Approver 人工批准主程序。
3. 用 SignPath 返回的已签名主程序执行 `tauri bundle`，生成 NSIS 安装包；该阶段不预生成 Updater 签名。
4. 上传安装包 GitHub Artifact，提交第二次 SignPath 请求并等待人工批准。
5. 校验最终安装包的 Authenticode 链和 `SignPath Foundation` 发布者，然后重新生成 Tauri Updater `.sig`、`latest.json` 与 SHA-256。
6. GitHub 发布完成后，复用同一安装包与 `.sig` 同步 Gitee；更新清单只改写下载 URL，并发布到独立的 `nearweave-updates` 仓库，禁止分别重建安装包。

主程序与安装包的 Artifact Configuration 都必须限制 `ProductName=NearWeave`、版本字段与发布版本一致、`CompanyName=railgun20001`。所有请求必须来自 GitHub Artifact 和 GitHub 托管 Runner，不接受本机二进制直接上传。

仓库内的 `.signpath/artifact-configurations/nearweave-exe.xml` 与 `nearweave-nsis.xml` 是申请和配置时的源文件；在 SignPath 控制台建立对应配置后，把实际 slug 写入 GitHub Repository Variables。项目必须链接预定义的 GitHub.com Trusted Build System，并将 GitHub App 授权给 `railgun20001/nearweave`。

参考：[SignPath Foundation 条件](https://signpath.org/terms.html)、[GitHub 受信任构建系统](https://docs.signpath.io/trusted-build-systems/github)、[Artifact Configuration](https://docs.signpath.io/artifact-configuration/reference)。
