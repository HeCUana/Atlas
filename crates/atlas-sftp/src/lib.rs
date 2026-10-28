//! Atlas 作为 **SFTP 服务端**：让远端运维电脑通过 SFTP 连到 Atlas，
//! 管理 Atlas 所托管的各个服务器的文件。
//!
//! Atlas 在这个方向上**只做服务端**，不做客户端——远程电脑用
//! FileZilla / WinSCP / `sftp` 命令行连进来，Atlas 不主动连出去。
//!
//! ## 模块划分
//!
//! - [`server`]：SFTP 服务端（虚拟文件系统、协议处理、SSH 会话）；
//! - [`keygen`]：SSH 密钥对生成（供 `atlas keygen` 使用）。
//!
//! ## 选型约束
//!
//! - `russh` **必须启用至少一个加密后端 feature**（`aws-lc-rs` 或 `ring`），
//!   两个都不启用会编译失败。已在 `Cargo.toml` 中通过默认 feature 保证。
//! - `russh-sftp` 与 `russh` **版本强耦合**，需成对升级。
//!
//! ## 安全约定
//!
//! - **只接受公钥认证**，口令认证一律拒绝；
//! - 客户端路径必须落在共享目录之内，越界一律拒绝；
//! - 授权名单在启动时读入，日志与错误信息不含口令、私钥等机密内容。

pub mod keygen;
pub mod server;

pub use keygen::{KeyFormat, KeyPair, generate_ed25519, generate_ed25519_with};
