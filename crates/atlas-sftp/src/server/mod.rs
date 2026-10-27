//! SFTP **服务端**：让远端运维电脑通过 SFTP 连到 Atlas，
//! 管理 Atlas 所托管的各个服务器的文件。
//!
//! 与 [`crate::client`]（Atlas 作为客户端连出去）方向相反。
//!
//! ## 虚拟文件系统
//!
//! SFTP 客户端只认 POSIX 路径，而 Atlas 托管的服务器目录在 Windows 上是
//! `C:\...` 形式。因此服务端对外呈现一棵**虚拟树**：
//!
//! ```text
//! /                     虚拟根：列出所有共享
//! /survival             → <share path>/survival
//! /survival/plugins     → <share path>/survival/plugins
//! /survival/plugins/x.jar
//! ```
//!
//! 共享（share）默认从 `[[servers]]` 自动派生：服务器的 `id` 作为虚拟目录名，
//! `workdir` 作为真实目录。也可以用 `[[sftp_server.share]]` 显式补充。
//!
//! ## 越界防护
//!
//! 客户端路径先做**词法**规整（`.` 丢弃、`..` 回退），`..` 不能弹出虚拟根；
//! 落到真实路径后，路径必定在对应 share 的目录之内。
//!
//! ## 授权公钥
//!
//! 服务端只接受公钥认证。授权名单放在 `authorized_keys` 格式的文件里
//! （一行一个 `ssh-ed25519 AAAA... 备注`），路径由配置给出。
//! 文件不存在时由 [`ensure_authorized_keys`] 自动创建——内容**只有说明注释**，
//! 不含任何公钥，因此不会有客户端能连上，直到用户自己把公钥贴进去。

pub mod handler;
pub mod session;

use std::path::{Path, PathBuf};

use atlas_core::{AtlasError, Result};

pub use handler::SftpHandler;
pub use session::{
    AUTHORIZED_KEYS_TEMPLATE, SftpServer, SftpServerOptions, ensure_authorized_keys,
    load_authorized_keys,
};

/// 一个共享目录：客户端看到的虚拟名 ↔ 本机真实目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpShare {
    /// 虚拟路径名（客户端看到的目录名）。
    pub name: String,
    /// 真实目录（本机路径）。
    pub local_path: PathBuf,
    /// 是否只读——只读共享不接受写入、删除、改名与建目录。
    pub read_only: bool,
}

impl SftpShare {
    /// 构造一个可读写的共享。
    pub fn new(name: impl Into<String>, local_path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            local_path: local_path.into(),
            read_only: false,
        }
    }

    /// 标记为只读。
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
}

/// 客户端路径的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// 虚拟根：列出所有共享。
    Root,
    /// 某个共享内的具体路径。
    Inside {
        /// 共享在列表中的下标。
        share: usize,
        /// 真实本机路径。
        local_path: PathBuf,
    },
}

/// 虚拟文件系统：客户端 POSIX 路径 ↔ 本机真实路径的映射。
#[derive(Debug, Clone, Default)]
pub struct VirtualFs {
    shares: Vec<SftpShare>,
}

impl VirtualFs {
    /// 创建空的虚拟文件系统。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用给定的共享列表创建。
    pub fn with_shares(shares: Vec<SftpShare>) -> Self {
        Self { shares }
    }

    /// 全部共享。
    pub fn shares(&self) -> &[SftpShare] {
        &self.shares
    }

    /// 追加一个共享；重名会报错。
    pub fn add_share(&mut self, share: SftpShare) -> Result<()> {
        if share.name.trim().is_empty() {
            return Err(AtlasError::Sftp("共享目录名不能为空".into()));
        }
        if share.name.contains('/') || share.name.contains('\\') {
            return Err(AtlasError::Sftp(format!(
                "共享目录名不能包含路径分隔符: {}",
                share.name
            )));
        }
        if self.shares.iter().any(|s| s.name == share.name) {
            return Err(AtlasError::Sftp(format!("共享目录名重复: {}", share.name)));
        }
        self.shares.push(share);
        Ok(())
    }

    /// 按下标取共享。
    pub fn share(&self, index: usize) -> Option<&SftpShare> {
        self.shares.get(index)
    }

    /// 把客户端路径解析成真实路径。
    ///
    /// - 虚拟根（`/` 或空串）→ [`Resolved::Root`]；
    /// - `/<共享名>/...` → 该共享下的真实路径；
    /// - 未知的共享名 → 报错。
    ///
    /// `..` 只能回退到虚拟根，不能再往上。
    pub fn resolve(&self, client_path: &str) -> Result<Resolved> {
        let mut segments: Vec<&str> = Vec::new();
        for seg in client_path.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(AtlasError::Sftp(format!("路径 {client_path} 越出根目录")));
                    }
                }
                other => segments.push(other),
            }
        }

        if segments.is_empty() {
            return Ok(Resolved::Root);
        }

        let name = segments[0];
        let share = self
            .shares
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| AtlasError::Sftp(format!("未知的共享目录: {name}")))?;

        let mut local_path = self.shares[share].local_path.clone();
        for seg in &segments[1..] {
            local_path.push(seg);
        }
        Ok(Resolved::Inside { share, local_path })
    }

    /// 把本机真实路径反查成客户端虚拟路径。
    ///
    /// 用于 `realpath` 之类的反向查询；不在任何共享之下时返回 `None`。
    pub fn to_client_path(&self, local_path: &Path) -> Option<String> {
        for share in &self.shares {
            if local_path == share.local_path {
                return Some(format!("/{}", share.name));
            }
            if let Ok(rest) = local_path.strip_prefix(&share.local_path) {
                let rest = rest.to_string_lossy().replace('\\', "/");
                if rest.is_empty() {
                    return Some(format!("/{}", share.name));
                }
                return Some(format!("/{}/{}", share.name, rest.trim_start_matches('/')));
            }
        }
        None
    }
}
