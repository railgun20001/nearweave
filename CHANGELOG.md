# 更新日志

本项目遵循语义化版本号。

## 未发布

## 0.4.1 - 2026-08-03

- Windows 防火墙操作使用复用正式主程序的原生 Rust GUI helper，不执行 PowerShell 脚本或弹出终端窗口。
- 安装和升级不再主动申请防火墙权限；首次启动与设置页新增“启用局域网传输”操作，只有用户明确选择后才请求 UAC，未启用时仅运行蓝牙传输。
- 安装器和应用只管理 NearWeave 当前产品目录、配置、接收文件及防火墙规则，不扫描或导入其他产品数据。

## 0.4.0

- 产品名、工程名、包名、应用标识、事件、协议 magic、发现域、RFCOMM UUID、临时文件前缀和安装元数据使用 NearWeave 标识。
- 发布新的蓝青交织 N 图标、跨平台图标资源和无蓝牙符号的宣传图，中文副标题统一为“附近设备互传”。
- 协议标识为 `NWV1`、`NWL1`、`nearweave-discovery-v1`、`nearweave-lan-aead-v1` 和 `nearweave-pairing-code-v1`。
- 安装目录为 `%LOCALAPPDATA%\NearWeave`，接收目录为 `Downloads\NearWeave Received`。
- Gitee 作为首选更新源、GitHub 作为备用源；v0.4.0 为 Updater 已签名但 Authenticode 申请中的公开基线。
