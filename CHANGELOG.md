# 更新日志

本项目遵循语义化版本号。NearWeave 从 v0.4.0 建立新的公开版本线，不重发重置前版本。

## 未发布

- 安装迁移和防火墙操作改用复用正式主程序的原生 Rust GUI helper，不再执行 PowerShell 脚本或弹出终端窗口。
- 安装和升级不再主动申请防火墙权限；首次启动与设置页新增“启用局域网传输”操作，只有用户明确选择后才请求 UAC，未启用时仅运行蓝牙传输。

## 0.4.0

- 产品名、工程名、包名、应用标识、事件、协议 magic、发现域、RFCOMM UUID、临时文件前缀和安装元数据统一为 NearWeave。
- 发布新的蓝青交织 N 图标、跨平台图标资源和无蓝牙符号的宣传图，中文副标题统一为“附近设备互传”。
- 协议硬切换为 `NWV1`、`NWL1`、`nearweave-discovery-v1`、`nearweave-lan-aead-v1` 和 `nearweave-pairing-code-v1`，不解析或广播重置前标识。
- 保留设备 ID、Noise 身份、信任记录、设置和 Tauri Updater 密钥；双方升级后无需重新配对。
- 新增旧版当前用户安装的白名单式卸载迁移，以及两个旧应用数据目录的无覆盖合并迁移。
- 新安装目录为 `%LOCALAPPDATA%\NearWeave`，接收目录为 `Downloads\NearWeave Received`；旧接收文件保持原位并提供打开入口。
- Gitee 作为首选更新源、GitHub 作为备用源；v0.4.0 为 Updater 已签名但 Authenticode 申请中的公开基线。
