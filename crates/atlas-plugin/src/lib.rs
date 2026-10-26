//! Atlas 插件系统。
//!
//! 本 crate 负责**与宿主边界无关**的部分：清单解析、插件发现、实例编排、
//! 启停与热重载。具体的插件 ABI 形态仍未定稿（见 `docs/功能文档.md`
//! 与 README 的待确认信息）。
//!
//! ## 插件形态
//!
//! | 形态 | 目录特征 | 清单来源 |
//! | --- | --- | --- |
//! | 单文件插件 | 一个可执行 / 动态库文件 | 同名 `<stem>.toml`，否则根目录 `plugin.toml` |
//! | 打包插件 | `.atlas` 归档包 | 归档内部的 `plugin.toml` |
//! | 源码插件 | `*.rs` 源文件（开发态） | 同名 `<stem>.toml`，否则根目录 `plugin.toml`，都没有则用占位清单 |
//! | 文件夹插件 | 含 `plugin.toml` 的目录 | 目录内的 `plugin.toml` |
//!
//! 详见 [`loader`]（发现规则与单文件清单约定）与 [`package`]（`.atlas` 归档格式）。
//!
//! ## 生命周期顺序（硬性要求）
//!
//! 插件的热重载顺序固定为：
//!
//! ```text
//! 停止事件订阅 → 注销命令 → 释放句柄 → 卸载 → 加载新实例
//! ```
//!
//! 这条顺序在 [`lifecycle`] 中被显式建模，并由 [`registry`] 在启停与重载时驱动。
//!
//! ## ABI 风险登记（重要，未决）
//!
//! **Rust 没有稳定 ABI。** 具体地：
//!
//! - `cdylib` + `libloading` 的动态库加载路径**不保证**跨编译器版本兼容，
//!   **同版本同参数也不保证**兼容；
//! - 卸载 Rust 动态库还会残留静态状态 / 线程局部 / panic 运行时；
//! - 因此该路径必须在文档与日志中标注为**实验性**，并在 FFI 边界做 `catch_unwind`。
//!
//! **主线路径是 Workspace 重编译模型**：插件与 Atlas 同处一个 Cargo workspace，
//! 重载时重编译并重启 Atlas 进程。由于 **Minecraft 服务端是独立子进程，
//! 重启 Atlas 不会关闭游戏服务器**，该路径同样满足"不关服"的产品承诺，
//! 而且没有 ABI 风险。
//!
//! 本 crate 因此**不实现**真实的 `cdylib` 动态库加载：`libloading` 虽在依赖里，
//! 但调用路径尚未落地，`PluginLifecycle` 只是**宿主进程内**的普通 Rust trait。
//!
//! ## 源码插件的代价（如实说明）
//!
//! 源码插件（`.rs`）是**开发态**形态：运行时需要本机具备 Rust 工具链，
//! 编译耗时**秒级到分钟级**，依赖获取需要访问 crates.io。
//! 因此它**不是**生产态的热更新手段。

pub mod lifecycle;
pub mod loader;
pub mod manifest;
pub mod package;
pub mod registry;

pub use lifecycle::{
    LifecycleStep, NoopInstanceFactory, NoopLifecycle, PluginInstance, PluginInstanceFactory,
    PluginLifecycle, UNLOAD_ORDER,
};
pub use loader::{
    DiscoveredPlugin, MANIFEST_EXT, PACKAGE_EXT, PluginExtra, PluginLoader, SOURCE_EXT,
};
pub use manifest::{MANIFEST_FILE, PluginKind, PluginManifest};
pub use package::{PackageContent, read_package, write_package};
pub use registry::{PluginRegistry, PluginStatus};
