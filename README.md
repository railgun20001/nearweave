# NearWeave

**附近设备互传。** NearWeave 是一个面向附近设备的点对点共享工具。两台 Windows 电脑可以不连接蓝牙，直接通过加密局域网连接发送文件、浏览授权目录并下载文件、同步文本剪贴板；也可以使用 Windows RFCOMM，并在同网时自动升级到局域网高速链路。

![NearWeave 在附近设备之间传输文件](docs/assets/nearweave-transfer-hero.png)

> 当前为 Windows 预览版。代码经过本地自动化测试、严格 Clippy 和前端生产构建，但纯局域网、动态端口、防火墙和断线切换仍需在两台真实电脑间完成最终硬件验收。

## 当前能力

- NearWeave 每次启动默认恢复连接服务；“停止连接”会取消任务、断开全部设备并释放监听，“恢复连接”只恢复已经启用的传输方式和发现。
- 用户明确启用局域网传输后，同一子网或 Windows 热点通过 UDP `37991` 自动发现；多网卡逐一发送定向广播，也可手动输入 IP 查询或使用明确的 `IP:端口` 直连。
- TCP 优先监听 `37992`，端口被占用时自动改用动态端口，并在发现信息中发布实际端口。
- 纯局域网首次连接使用 Noise XX 加密握手，双方核对相同六位验证码；确认后记住设备身份并可在设置中撤销。
- 48 KiB 分块传输文件，接收完成后校验文件大小与 SHA-256。
- 文件统一保存到用户下载目录下的 `NearWeave Received`，不会采用远端提供的任意路径。
- 用户显式选择本机共享目录；对方逐层浏览直接子项并明确点击下载，不能写入或删除，也不会一次传递全部子孙文件。
- 对共享文件请求执行规范化路径检查，拒绝绝对路径、`..` 和越出授权目录的路径。
- 文本剪贴板双向同步默认开启；Windows 使用剪贴板变更事件按需唤醒，并通过内容哈希避免两台电脑无限回传。
- 首次启动会明确询问是否启用局域网传输，也可稍后在设置页启用；配置视图还可控制开机自启动。局域网选择与主界面的文本剪贴板开关会保存在当前用户配置中。
- 启动后优先通过 Gitee 检查更新，失败时自动回退 GitHub Releases；也可以在设置中手动检查、查看下载进度并安装经过签名验证的新版本。
- 最多同时连接 8 台设备；文件、拖拽和目录操作只发给当前选中设备，本机剪贴板同步给全部活动设备。
- 可以通过文件选择器发送多个文件，也可以把文件直接拖入应用窗口发送；拖拽只处理 drop 并带 1 秒防重。
- 发送方或接收方均可中断单条任务，取消控制消息不会被文件分块阻塞。
- 设备列表合并同一电脑的局域网和蓝牙能力；连接时优先局域网，失败或断开时自动回退 RFCOMM。
- 链路切换导致当前文件中断时，等待重新连接并从头自动重试当前文件；目录中已经完成的文件不会重复发送。
- 30 秒心跳、90 秒失联判定，以及按 1、2、5、10、30 秒退避并封顶 30 秒的自动重连。
- 连接状态、传输进度、错误提示和带对端通知的主动断开。
- 空闲时读写任务阻塞等待事件；只有心跳和连接维护保持运行，文件分块、哈希与进度更新只在传输期间执行。

当前剪贴板同步仅支持 UTF-8 文本，单条上限 512 KiB。图片、富文本和剪贴板文件列表尚未实现。

## 两台 Windows 电脑使用

1. 两台电脑连接同一家庭/工作局域网，或让一台开启 Windows 热点并由另一台接入，然后启动 NearWeave。
2. 首次启动时在两台电脑上选择“启用局域网传输”，并确认 Windows UAC；也可以先跳过，之后在设置页执行同一操作。未启用时不会广播、监听或连接局域网。
3. NearWeave 连接服务默认运行；如曾在本次运行中停止，请点击“恢复连接”。另一端会自动发现，也可以输入对方 IP。
4. 首次纯局域网连接时，在两台电脑上核对六位验证码并分别确认。
5. 连接后可以选择文件发送、拖入文件、授权共享目录，或暂停、恢复文本剪贴板同步。
6. 如需无网络蓝牙传输，先在 Windows“设置 → 蓝牙和设备”中完成系统配对，再从同一设备卡片连接。
7. 设置页可管理局域网传输、开机自启动、软件更新和已信任设备。

附近设备会同时显示自动发现的局域网设备和 Windows 已配对蓝牙设备，并通过稳定设备 ID 合并同一设备的两种能力。局域网广播被访客网络、AP 隔离或 VLAN 阻断时，可输入 IP；如果 UDP 查询也被阻断，则输入对方界面显示的 `IP:端口`。

安装和升级不会请求防火墙管理员授权。只有用户在首次启动提示或设置页选择“启用局域网传输”时，NearWeave 才会通过无控制台窗口的原生 Rust GUI helper 请求 Windows UAC，并添加仅限 IPv4 本地子网来源的 UDP `37991` 和动态 TCP 入站规则。规则适用于所有网络配置文件但禁止 Edge Traversal；取消授权不会影响基础蓝牙传输。卸载时仅在检测到这些规则后请求授权清理。

## 开发

环境要求：

- Windows 10/11
- Node.js 20 或更高版本
- pnpm 11
- Rust MSVC 工具链
- Visual Studio 2022“使用 C++ 的桌面开发”
- Microsoft Edge WebView2

```powershell
pnpm install
pnpm tauri dev
```

前端检查：

```powershell
pnpm build
```

开发态 UI 冒烟测试（先在另一终端运行 `pnpm dev -- --host 127.0.0.1`）：

```powershell
python tests/ui_smoke.py
```

Rust 检查：

```powershell
cd src-tauri
cargo fmt --all --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

构建仅供本机测试、无需生成 Updater 签名的 Windows 安装包：

```powershell
pnpm tauri build --no-sign
```

安装包目标为 NSIS。构建结果位于 `src-tauri/target/release/bundle/nsis/`。

正式发布必须使用长期保管的 Tauri Updater 私钥构建。私钥不得放入仓库；本机默认路径为 `$env:USERPROFILE\.tauri\nearweave.key`：

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY="$env:USERPROFILE\.tauri\nearweave.key"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD=Get-Content -LiteralPath "$env:USERPROFILE\.tauri\nearweave.key.password" -Raw
pnpm tauri build
.\scripts\verify-installer.ps1
Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY,Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
```

GitHub Actions 发布前，需要在仓库 Actions Secrets 中创建 `TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 和 `GITEE_TOKEN`。前两个值分别为上述私钥文件和口令文件的完整内容；`GITEE_TOKEN` 只维护 `railgun20001/nearweave` 的代码与 Release，以及 `railgun20001/nearweave-updates` 的稳定更新清单。推送与应用版本一致的 `v<版本号>` 标签后，工作流会先发布 GitHub NSIS 安装包、签名和 `latest.json`，再把同一安装包与签名同步到 Gitee，并把改写为 Gitee 下载地址的清单发布到独立更新仓库。

`.github/workflows/sync-gitee.yml` 也支持手动输入已有的 Release 标签进行补同步。Gitee 主仓库的 `main` 和标签不得直接提交；同步发现非快进分叉时会失败，不会静默覆盖 Gitee 独有提交。机器维护的 `nearweave-updates` 仓库只在 `main` 保存稳定的 `latest.json`。

## AI 开发说明

本项目由用户定义产品目标、功能范围和验收方向，主要代码、界面、测试与文档由 OpenAI Codex 协助生成和迭代。AI 生成内容已经过自动化检查和人工可追溯的 Git 提交管理，但这不等同于所有硬件组合均已验证；欢迎通过 GitHub Issue 报告蓝牙适配器、Windows 版本和驱动相关问题。

## Code signing policy

Free code signing provided by SignPath.io, certificate by SignPath Foundation.

项目正在申请 SignPath Foundation 免费开源代码签名。获批后，Windows Authenticode 会显示证书发布者 `SignPath Foundation`；GitHub 用户名 `railgun20001` 作为项目维护者和安装包制造商元数据，不会替代证书中的法律发布者名称。申请获批前发布的安装程序仍可能显示“未知发布者”。

签名范围、受信任构建、人工审批和团队角色见 [Code signing policy](CODE_SIGNING_POLICY.md)。软件的网络通信、本地数据和第三方更新服务见 [隐私政策](PRIVACY.md)，安全问题请使用 [私密漏洞报告](SECURITY.md)。

## 架构

```text
React/TypeScript 界面
          │ Tauri commands / events
          ▼
Rust 应用服务
 ├─ protocol.rs       平台无关的消息与二进制帧
 ├─ files.rs          文件、共享目录、校验和路径隔离
 ├─ clipboard.rs      剪贴板监听与回环抑制
 ├─ state.rs          连接、目录、传输和 UI 快照
 └─ transport/
     ├─ windows.rs    当前 Windows RFCOMM 实现
     ├─ network.rs    Noise 纯局域网与 RFCOMM 授权高速链路
     └─ unsupported.rs 其他平台的明确占位
```

协议和文件规则不依赖 Windows API。后续平台只应新增传输适配器，不应在业务层复制一套消息协议。详细设计见 [架构说明](docs/ARCHITECTURE.md) 和 [传输协议](docs/PROTOCOL.md)。

## 安全边界

- 纯局域网使用 `Noise_XX_25519_ChaChaPoly_SHA256`；首次连接要求双方核对验证码，后续同时校验设备 ID 与静态公钥。
- Windows 静态私钥通过当前用户范围 DPAPI 加密保存；信任记录可以在设置中撤销。
- 兼容高速链路的临时密钥仍只通过已认证 RFCOMM 交换；UDP 广播不携带密钥。
- UDP 发现信息不作为身份凭据，TCP 建链必须重新完成 Noise 或当前蓝牙会话授权校验。
- 连接建立后会自动接收对方发送的文件，但只能写入固定接收目录。
- 共享目录不会跨重启自动恢复，避免用户忘记已经授权的目录。
- 目录索引跳过符号链接，文件下载前会再次解析真实路径并确认仍位于授权根目录。
- 防火墙规则只针对 NearWeave 程序和 IPv4 本地子网；规则适用于所有网络配置文件，以支持 Windows 热点，同时禁止 Edge Traversal。
- 局域网服务默认不启动；只有用户明确启用且防火墙规则校验通过后，应用才绑定 UDP/TCP 监听并持久化该选择。

## 跨平台方向

Tauri 2 可以承载 Windows、macOS、Linux、Android 和 iOS 界面，但各系统的蓝牙能力不同：

- Windows：当前使用 RFCOMM。
- Android：可实现 Bluetooth Classic RFCOMM 适配器。
- macOS/Linux：分别接入系统蓝牙框架与 BlueZ，并保持相同应用协议。
- iOS：不能把通用 RFCOMM 作为默认路线，需要 BLE GATT 或“蓝牙发现/认证 + 局域网数据通道”。

因此，大文件和大目录的长期方向是保留蓝牙发现与近场信任，同时允许协商到局域网传输；RFCOMM 继续作为无网络环境下的基础通道。

## 安装与发布

从 [Gitee Releases](https://gitee.com/railgun20001/nearweave/releases) 或 [GitHub Releases](https://github.com/railgun20001/nearweave/releases) 下载 `nearweave_0.4.1_x64-setup.exe`。Gitee 是首选更新源，GitHub 是备用源；两端的安装包与 `.sig` 字节一致。两个 `latest.json` 来自同一次构建并包含相同版本和安装包签名，但分别保留 Gitee、GitHub 下载 URL，才能在首选源故障时真正回退。

Windows 安装器只管理 NearWeave 当前产品目录、安装项、开始菜单项和桌面快捷方式，不检测、卸载或导入其他产品数据。防火墙操作复用已签名的 NearWeave 主程序作为原生 Rust GUI helper，不调用 PowerShell，也不会弹出终端窗口；规则只在用户从应用内明确启用局域网传输时创建。

应用数据保存在 `io.github.railgun20001.nearweave`，接收目录为 `Downloads\NearWeave Received`。安装器不会扫描或导入其他应用目录、身份、信任记录、设置和接收文件。

v0.4.1 安装包具有 Tauri Updater 签名，但在 SignPath Foundation 申请获批前没有 Authenticode，SmartScreen 可能显示未知发布者。申请获批后，后续版本将分别签名主程序和安装包，再为最终安装包生成 Updater `.sig` 与 `latest.json`。详见 [SignPath 发布说明](docs/SIGNPATH.md)。

Updater 私钥、公钥和口令是正式更新链的长期密钥材料。私钥或口令一旦丢失，已安装客户端将无法验证后续更新；维护者必须对 `$env:USERPROFILE\.tauri\nearweave.key` 和同目录的 `.key.password` 做离线备份，仓库只保存公开公钥。

## 许可证

NearWeave 以 [Apache License 2.0](LICENSE) 发布。依赖组件继续遵循各自的开源许可证。
