//! Atlas 远程文件管理（SFTP）。
//!
//! 通过 SFTP 管理 Atlas 所托管的每个服务器中的文件。
//!
//! ## 模块划分
//!
//! - [`client`]：SFTP 客户端（连接、列举、上传下载、删除、建目录）；
//! - [`credentials`]：认证凭据，敏感字段在 `Debug` 中脱敏；
//! - [`hostkey`]：SSH 主机密钥校验（known_hosts），防中间人攻击；
//! - [`path`]：路径拼接与本地映射的**纯函数**，是本 crate 的测试主体。
//!
//! ## 选型约束
//!
//! - `russh` **必须启用至少一个加密后端 feature**（`aws-lc-rs` 或 `ring`），
//!   两个都不启用会编译失败。已在 `Cargo.toml` 中通过默认 feature 保证。
//! - `russh-sftp` 与 `russh` **版本强耦合**，需成对升级。
//!
//! ## 安全约定
//!
//! - 连接时强制校验服务端主机密钥，未知主机默认拒绝；
//! - 日志与错误信息不含口令、私钥等机密内容。

pub mod client;
pub mod credentials;
pub mod hostkey;
pub mod path;

pub use client::{RemoteEntry, SftpClient, SftpConnectOptions};
pub use credentials::SftpCredentials;
pub use hostkey::{HostKeyDecision, KnownHostEntry, UnknownHostPolicy};
pub use path::{human_size, is_hidden, join_remote, normalize_remote, sanitize_local_path};
