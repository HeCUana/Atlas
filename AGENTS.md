# Atlas — 工程约定

> 本文件是 Atlas workspace 的**强制约定**。任何修改代码的 AI 代理或人类贡献者都必须遵守。
> 与本文件冲突的写法一律视为缺陷。

## 1. 项目定位

Atlas 是用 Rust 编写的 Minecraft **多服务器管理工具**。

- **本体精简**：不内置任何具体业务逻辑（榜单、经济、任务、备份策略等一律外置为插件）。
- **多服务器能力直给**：子进程托管、批量开关服、SFTP 文件管理、日志解析、命令执行、远程 TUI 属于本体能力。
- **扩展靠插件**：业务功能通过 Atlas Plugin 调用 Atlas 接口实现。

详细设计见 [`README.md`](README.md)，功能范围见 [`docs/功能文档.md`](docs/功能文档.md)。

## 2. 工作区结构

```
crates/
  atlas-core/    共享类型层：事件、命令树、权限模型、配置   ← 契约基座
  atlas-parser/  日志识别与解析（当前重点）
  atlas-server/  子进程托管、生命周期、终端命令执行
  atlas-plugin/  插件清单、发现、注册表、热重载编排
  atlas-sftp/    远程文件管理
  atlas-tui/     终端界面
  atlas-cli/     二进制入口（atlas）
```

### 2.1 依赖方向（硬性）

```
atlas-cli  →  atlas-core
                ↑    ↑    ↑    ↑
   atlas-parser  atlas-server  atlas-plugin  atlas-sftp  atlas-tui
```

- **`atlas-core` 不得依赖任何其他内部 crate**。它是所有模块的共享词汇表。
- 内部 crate 之间**不得相互依赖**（`atlas-parser` 不能引用 `atlas-server`）。
  需要协作时通过 `atlas-core` 的类型或由 `atlas-cli` 组装。
- 新增内部依赖前必须先修改本节并说明理由。

### 2.2 依赖版本

- 所有第三方依赖在**根 `Cargo.toml` 的 `[workspace.dependencies]`** 中统一声明。
- 各 crate 的 `Cargo.toml` 一律用 `xxx.workspace = true` 引用，**禁止**在 crate 内写具体版本号。
- 新增依赖时先加到根 `Cargo.toml`，再在 crate 中引用。

## 3. 工具链与代码规范

- **edition 2024**，MSRV 1.88（见根 `Cargo.toml` 的 `edition` / `rust-version`）。
- 工具链版本由 `rust-toolchain.toml` 固定，**不要**改动它。
- 全量通过 `cargo clippy --workspace --all-targets -- -D warnings`。
- 格式化用 `cargo fmt --all`。
- **禁止 `unwrap()` / `expect()`**（测试代码除外）：一律用 `?` 配 `AtlasError` 传播。
- **禁止 `panic!()`**（测试代码除外）。
- 错误类型统一用 `atlas_core::AtlasError`，`atlas-core` 之外的 crate 不要自造错误枚举。
- 面向用户/运维的错误信息用**中文**。
- 公开 API 写 `///` 文档注释；模块写 `//!` 模块级说明。
- 关键取舍、风险、已知限制要在代码注释或文档里**如实写明**，不要写成"已解决"。

## 4. 关键设计约束（违反即缺陷）

这几条来自 `README.md` 的风险登记册，实现时必须遵守：

### 4.1 日志解析

- **解析失败不丢行**：任何一行输入都必须产出且仅产出一条事件；无法识别时降级为
  `LogLevel::Unknown` + `message` = 整行 + `raw` = 整行。
- **原始行始终保留**：`raw` 字段保留未经处理的原文。
- 解析前必须**剥离 ANSI 转义序列**。
- 日志 profile **可配置**，不硬编码在二进制里。

### 4.2 事件总线

- 底层是 `tokio::sync::broadcast`，它满时淘汰最旧消息、落后时**静默丢消息**。
- 消费侧**必须显式处理** `RecvError::Lagged`，不能把它当正常返回忽略。
  使用 `atlas_core::EventBus::recv` 返回的 `RecvOutcome::Lagged`。

### 4.3 权限

- **权限判定绝不能基于消息文本。** 判定输入必须是结构化的
  `Subject` / `Action` / `Resource` 三元组。
- 默认拒绝：未显式授予的组合一律不允许。
- 每个能产生副作用的入口都要独立校验（开关服、SFTP、命令树、TUI 控制、备份）。

### 4.4 子进程

- stdout 与 stderr **必须由两个独立任务并发读取**（否则高日志量下管道写满 → 死锁）。
- 行分隔必须同时处理 `\n` 与 `\r`（Minecraft 进度条用 `\r` 覆盖同一行）。
- Atlas 退出前要先把停止命令投递给子进程并等待其优雅退出，超时再强制终止。

### 4.5 插件热重载

- **Rust 没有稳定 ABI**。`cdylib` + `libloading` 路径**不保证**跨编译器版本兼容，
  同版本同参数也不保证。实现该路径时必须在文档与日志中标注为实验性，
  并在 FFI 边界做 `catch_unwind`。
- 主线路径是 **Workspace 重编译模型**：插件与 Atlas 同处一个 workspace，
  重载时重编译并重启 Atlas 进程。由于 Minecraft 服务端是独立子进程，
  **重启 Atlas 不会关闭游戏服务器**。
- 插件生命周期顺序必须是：停止事件订阅 → 注销命令 → 释放句柄 → 卸载 → 加载新实例。

### 4.6 TUI

- 异常退出时必须恢复终端状态（raw mode / alternate screen / 光标可见性）。
  使用 `ratatui::init()` / `ratatui::restore()`，或 `ratatui::run()`。

### 4.7 SFTP 服务端（Atlas 作为服务端）

- Atlas 的 SFTP 能力**只做服务端方向**：远端电脑用 SFTP 连进来管理受管服务器的文件。
  不提供「Atlas 主动连出去」的客户端命令。
- 服务端把客户端看到的 POSIX 路径映射到**受管服务器的真实目录**，映射与越界防护
  由 `atlas_sftp::server::VirtualFs` 负责，**不得**在 handler 里另写一套。
- **读到文件末尾必须回 `StatusCode::Eof` 状态包，不能回空的 DATA 包。**
  OpenSSH 客户端不把空 DATA 当作结束，会以同一个 offset 无限重复请求——
  下载卡死，服务端 CPU 还会被烧满（实测 3 秒涨 2.7 秒）。
- 服务端**只接受公钥认证**；认证失败要走固定耗时，避免时序探测。
- 服务端主机密钥与客户端授权公钥是**两把不同的钥匙**：
  主机密钥放 `keys/host_ed25519`，客户端公钥放 `keys/authorized_keys`。
  不要把主机私钥当成客户端密钥用。
- **授权公钥文件缺失时自动创建，但绝不覆盖已有文件**
  （`atlas_sftp::server::ensure_authorized_keys`）。自动创建的内容**只有说明注释，
  不含任何公钥**——新建即「谁都进不去」；自动生成一份「谁都能进」的授权名单
  比谁都进不去危险得多。已有的公钥一行都不能丢。
- 授权名单在**启动时读入**，往文件里贴完公钥要重启 `atlas serve` 才生效；
  空名单时必须明确告警，不能让人以为"服务正常但连不上"是配置问题。
- `atlas-sftp` 可以依赖 `atlas-core`（配置类型、错误类型统一由 core 提供），
  但**不得依赖其他内部 crate**（见 §2.1）。

## 5. 测试要求

- 每个 crate 至少要有单元测试；`atlas-parser`、`atlas-server` 必须有覆盖真实日志样本的测试。
- 集成测试放在各 crate 的 `tests/` 目录。
- 测试不得依赖网络；需要 SFTP 等外部依赖的逻辑用纯函数拆分后单测。
- 提交前必须 `cargo test --workspace` 全绿。

## 6. 提交规范

- 提交信息用中文，格式：`feat(parser): 支持 CraftBukkit 家族日志解析`。
- 一次提交只做一件事。
- 不提交 `target/`、`.env`、本地配置文件（`atlas.toml`）。
