# NearWeave 项目约定

- Git 提交信息和代码注释默认使用中文。
- 每完成一个可独立验证的功能，应检查 `git status --short`、暂存区差异和忽略规则，并单独提交；不得混入无关改动。
- 提交前应使用显式路径暂存文件，执行 `git diff --cached --check`，并运行与本次改动相匹配的检查。
- `src-tauri/src/protocol.rs` 必须保持平台无关，不得导入 Windows、Android、Apple 或 BlueZ API。
- 平台蓝牙实现只能放在 `src-tauri/src/transport/`；新增平台不得复制文件、目录或剪贴板业务协议。
- iOS 路线不得假设通用 RFCOMM 可用，应设计 BLE GATT 或局域网协商通道。
- 所有远端文件名必须清理；共享文件请求必须先规范化并验证仍位于用户授权根目录。
- 不得持久化共享目录白名单，除非同时提供清晰的授权恢复与撤销界面。
- 删除失败传输的临时文件时，只能删除本次传输创建并记录的 `.nearweave-<UUID>.part` 文件。
- 修改协议必须同步 `docs/PROTOCOL.md` 并增加协议编解码测试。
