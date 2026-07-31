# NearWeave：附近设备互传

![NearWeave 在附近设备之间传输文件](assets/nearweave-transfer-hero.png)

NearWeave（NearWeave）是一款面向附近 Windows 电脑的点对点共享工具。两台电脑完成系统蓝牙配对后，即可发送文件、浏览授权目录并下载文件，还能按需同步文本剪贴板。

当双方处于同一局域网时，NearWeave 会在已认证的蓝牙连接之上自动协商加密 TCP 通道，让文件和目录数据优先走局域网；局域网不可用或连接中断时，则自动回退到 Bluetooth Classic RFCOMM。用户不需要手动选择传输方式。

> **一句话介绍：** NearWeave 用蓝牙建立近场信任，用局域网承载高速数据，让两台 Windows 电脑像隔空搭了一座桥。

## 为什么选择 NearWeave

- **连接自然：** 使用 Windows 已完成配对的蓝牙设备，不依赖账号体系。
- **同网加速：** 检测到双方处于同一局域网后，自动优先使用加密 TCP 链路。
- **无网可用：** 没有可用局域网时，仍能通过 Bluetooth Classic RFCOMM 完成基础传输。
- **文件完整：** 文件分块传输，接收完成后校验文件大小和 SHA-256。
- **目录可控：** 只有用户本次明确授权的目录可以被浏览和下载，对方不能写入或删除。
- **剪贴板同步：** 支持双向同步 UTF-8 文本，并通过内容哈希抑制循环回传。
- **状态清晰：** 界面显示当前使用蓝牙还是局域网，并展示进度、平均速率、耗时和预计剩余时间。

## 它是怎样工作的

```mermaid
flowchart LR
    A["Windows 系统蓝牙配对"] --> B["Bluetooth Classic RFCOMM<br/>建立认证连接"]
    B --> C{"双方是否处于<br/>同一局域网"}
    C -->|是| D["ChaCha20-Poly1305<br/>加密 TCP 高速链路"]
    C -->|否| E["RFCOMM 基础传输"]
    D --> F["文件、目录与剪贴板数据"]
    D -.链路不可用时自动回退.-> E
    E --> F
```

蓝牙承担设备发现、身份信任和连接控制；局域网承担大部分数据传输。局域网会话密钥只通过已认证的蓝牙连接交换，不会出现在 UDP 发现广播中。

## 理论传输速率

先说明单位：

- `Mb/s` 表示每秒兆比特，常用于标注网络链路速率；
- `MB/s` 表示每秒兆字节，常用于显示文件传输速度；
- 理想换算关系为 `1 MB/s = 8 Mb/s`；
- 下表耗时按 `1 GiB = 1,073,741,824 字节`计算，不包含任何协议、加密、无线竞争、磁盘和系统开销。

| 传输链路 | 标称或规格上限 | 理想文件速率 | 传输 1 GiB 的纯理论最短耗时 | 相对 3 Mb/s EDR PHY |
| --- | ---: | ---: | ---: | ---: |
| Bluetooth Classic EDR 物理层 | 3 Mb/s | 0.375 MB/s | 约 47 分 43 秒 | 1 倍 |
| Bluetooth 3-DH5 单向 ACL 用户载荷 | 2.1781 Mb/s | 约 0.272 MB/s | 约 65 分 44 秒 | 约 0.73 倍 |
| 百兆局域网 | 100 Mb/s | 12.5 MB/s | 约 85.9 秒 | 约 33 倍 |
| 千兆局域网 | 1,000 Mb/s | 125 MB/s | 约 8.6 秒 | 约 333 倍 |

Bluetooth SIG 给出的 Bluetooth Classic EDR 8DPSK 物理层总空口速率为 3 Mb/s；在 3-DH5 数据包下，规格列出的理想单向 ACL 用户载荷上限为 2,178.1 kb/s。RFCOMM、L2CAP、加密和 NearWeave 应用协议仍会产生额外开销，因此实际文件速度一定低于这些理论值。

百兆和千兆局域网一栏同样只是链路标称速率的数学换算，并不代表 NearWeave 承诺达到 12.5 MB/s 或 125 MB/s。真实速度还会受到以下因素影响：

- 两台电脑中较慢一端的网卡和协商速率；
- 2.4 GHz 或 5 GHz 频段、信号强度、距离和无线干扰；
- 两台无线设备是否需要通过同一接入点重复占用空口；
- 路由器性能、访客网络隔离、防火墙、VPN 和虚拟网卡；
- 磁盘读写、杀毒软件扫描、文件数量、CPU 加密与哈希速度；
- NearWeave 当前版本的分块大小、内存复制和界面状态更新频率。

因此，准确的推广表述是：

> 在千兆局域网中，链路标称带宽是 Bluetooth Classic EDR 3 Mb/s 物理层的约 333 倍。NearWeave 会自动优先使用加密局域网链路，但实际文件传输速度以界面显示和双机实测为准。

不建议使用“NearWeave 传输速度可达 125 MB/s”作为宣传语，除非已经在明确的硬件、网络和文件条件下完成可复现的真机测试。

## 安全与边界

- RFCOMM 使用 Windows 蓝牙认证和加密能力。
- 局域网一次性密钥只经当前蓝牙会话交换。
- TCP 中的每个应用协议帧都经过 ChaCha20-Poly1305 认证加密。
- 接收文件先写入本次传输专属临时文件，大小和 SHA-256 校验通过后才保存为最终文件。
- 远端文件名会被清理，接收文件只能进入固定接收目录。
- 共享目录不跨应用重启自动恢复，避免用户遗忘仍在生效的授权。

NearWeave 当前是 Windows 预览版。正式使用前，建议在实际使用的两台电脑之间验证蓝牙连接、局域网升级、防火墙策略、断线回退和大文件传输。

## 三步开始传输

1. 在两台 Windows 电脑的系统设置中完成蓝牙配对，并分别启动 NearWeave。
2. 在接收连接的一端开启“NearWeave 连接”，另一端扫描并连接该设备。
3. 选择或拖入文件开始发送；如果双方处于同一局域网，NearWeave 会自动启用高速链路。

首次启用同网传输时，Windows 防火墙可能询问是否允许访问网络。只应在可信的家庭或工作专用网络中允许。

## 适合这些场景

- 在两台办公电脑之间快速传递文档、图片和安装包；
- 临时共享一个只读目录，让对方按需下载文件；
- 在附近电脑之间同步地址、命令和短文本；
- 无法使用网盘、没有互联网，或者不希望先上传到第三方服务器；
- 希望局域网断开后仍保留蓝牙基础传输能力。

## 可直接发布的短版文案

### 社交平台版

NearWeave 是一款附近 Windows 电脑点对点共享工具：蓝牙配对后即可发送文件、浏览授权目录和同步文本剪贴板；双方处于同一局域网时，会自动升级到加密 TCP 高速链路，网络不可用时再回退蓝牙。无需账号，文件数据不需要先上传第三方云端，连接状态、传输速率和文件校验结果都清晰可见。

### 一句话版

**NearWeave——用蓝牙建立信任，用局域网加速传输。**

## 速率资料来源

- [Bluetooth SIG：BR/EDR Radio Physical Layer Specification](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core_v6.3/out/en/br-edr-controller/radio-physical-layer-specification.html)
- [Bluetooth SIG：BR/EDR Baseband Specification](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core-61/out/en/br-edr-controller/baseband-specification.html)
- [Microsoft Learn：Bluetooth RFCOMM](https://learn.microsoft.com/en-us/windows/apps/develop/devices-sensors/send-or-receive-files-with-rfcomm)
