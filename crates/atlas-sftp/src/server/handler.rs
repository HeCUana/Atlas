//! SFTP 协议处理器：把客户端请求映射到本机文件操作。
//!
//! 实现 [`russh_sftp::server::Handler`]。路径一律先经 [`VirtualFs::resolve`]
//! 映射到本机真实路径，再落到文件系统上——客户端**永远**碰不到 share 之外的东西。

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};

use super::{Resolved, VirtualFs};

/// 一次 `readdir` 最多返回多少条目。
const READDIR_BATCH: usize = 64;

/// 已打开的文件。
struct OpenFile {
    /// 本机真实路径。
    path: PathBuf,
    /// 是否以可写方式打开。
    writable: bool,
}

/// 目录读取游标。
struct DirCursor {
    /// 剩余条目。
    entries: Vec<File>,
    /// 下一个要返回的条目下标。
    index: usize,
}

/// SFTP 协议处理器。
pub struct SftpHandler {
    /// 虚拟文件系统。
    fs: VirtualFs,
    /// 打开的文件句柄。
    files: HashMap<String, OpenFile>,
    /// 目录句柄。
    dirs: HashMap<String, DirCursor>,
    /// 句柄自增 id。
    next_id: u64,
}

impl SftpHandler {
    /// 创建处理器。
    pub fn new(fs: VirtualFs) -> Self {
        Self {
            fs,
            files: HashMap::new(),
            dirs: HashMap::new(),
            next_id: 1,
        }
    }

    /// 生成一个新的句柄键。
    fn new_handle(&mut self, kind: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{kind}{id}")
    }

    /// 解析客户端路径为真实路径；虚拟根视为错误（不能把根当文件用）。
    fn resolve_local(&self, client_path: &str) -> Result<PathBuf, StatusCode> {
        match self.fs.resolve(client_path) {
            Ok(Resolved::Inside { local_path, .. }) => Ok(local_path),
            Ok(Resolved::Root) => Err(StatusCode::Failure),
            Err(_) => Err(StatusCode::NoSuchFile),
        }
    }

    /// 解析客户端路径，并同时返回它所属共享的下标。
    fn resolve_with_share(&self, client_path: &str) -> Result<(PathBuf, usize), StatusCode> {
        match self.fs.resolve(client_path) {
            Ok(Resolved::Inside { share, local_path }) => Ok((local_path, share)),
            Ok(Resolved::Root) => Err(StatusCode::Failure),
            Err(_) => Err(StatusCode::NoSuchFile),
        }
    }

    /// 目标共享是否只读。
    fn is_read_only(&self, share: usize) -> bool {
        self.fs.share(share).is_some_and(|s| s.read_only)
    }
}

/// 把 IO 错误映射成 SFTP 状态码。
fn io_status(err: &std::io::Error) -> StatusCode {
    match err.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => StatusCode::Failure,
        _ => StatusCode::Failure,
    }
}

/// 由元数据构造 SFTP 属性。
fn attrs_of(meta: &fs::Metadata) -> FileAttributes {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32);
    let atime = meta
        .accessed()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32);
    FileAttributes {
        size: Some(meta.len()),
        uid: None,
        user: None,
        gid: None,
        group: None,
        // POSIX 权限位；Windows 上退化成粗粒度标记。
        permissions: Some(if meta.is_dir() { 0o040755 } else { 0o100644 }),
        atime,
        mtime,
    }
}

/// 构造一个目录条目。
fn dir_entry(name: &str, meta: &fs::Metadata) -> File {
    File::new(name.to_owned(), attrs_of(meta))
}

/// 创建符号链接。
///
/// Windows 上创建符号链接需要特权，这里明确返回"不支持"，而不是给出
/// 含糊的失败。
#[cfg(unix)]
fn create_symlink(link: &std::path::Path, target: &str) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_link: &std::path::Path, _target: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "Windows 上创建符号链接需要特权，本服务端不支持",
    ))
}

impl russh_sftp::server::Handler for SftpHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: std::collections::HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        tracing::debug!(id, %filename, ?pflags, "SFTP open");
        let (local_path, share) = self.resolve_with_share(&filename)?;

        let wants_write = pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::APPEND);
        if wants_write && self.is_read_only(share) {
            return Err(StatusCode::PermissionDenied);
        }

        let mut options = fs::OpenOptions::new();
        options.read(true);
        if wants_write {
            options.write(true);
        }
        if pflags.contains(OpenFlags::CREATE) {
            options.create(true);
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            options.truncate(true);
        }
        if pflags.contains(OpenFlags::EXCLUDE) {
            options.create_new(true);
        }
        if pflags.contains(OpenFlags::APPEND) {
            options.append(true);
        }

        let file = options.open(&local_path).map_err(|e| io_status(&e))?;
        drop(file);

        let handle = self.new_handle("f");
        self.files.insert(
            handle.clone(),
            OpenFile {
                path: local_path,
                writable: wants_write,
            },
        );
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.files.remove(&handle);
        self.dirs.remove(&handle);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        tracing::debug!(id, %handle, offset, len, "SFTP read");
        let Some(open) = self.files.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let path = open.path.clone();
        let mut file = fs::File::open(&path).map_err(|e| io_status(&e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(&e))?;
        let mut buf = vec![0u8; len as usize];
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = file.read(&mut buf[filled..]).map_err(|e| io_status(&e))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        buf.truncate(filled);
        if filled == 0 {
            // 读到文件末尾：必须回 **EOF 状态包**，不能回空的 DATA 包。
            // OpenSSH 客户端不把空 DATA 当作结束，会以同一个 offset 无限重复请求——
            // 既让下载卡死，又把服务端 CPU 烧满。
            return Err(StatusCode::Eof);
        }
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let Some(open) = self.files.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        if !open.writable {
            return Err(StatusCode::PermissionDenied);
        }
        let path = open.path.clone();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| io_status(&e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(&e))?;
        file.write_all(&data).map_err(|e| io_status(&e))?;
        file.flush().map_err(|e| io_status(&e))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let local = self.resolve_local(&path)?;
        let meta = fs::symlink_metadata(&local).map_err(|e| io_status(&e))?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let Some(open) = self.files.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let meta = fs::metadata(&open.path).map_err(|e| io_status(&e))?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let (local, share) = self.resolve_with_share(&path)?;
        if self.is_read_only(share) {
            return Err(StatusCode::PermissionDenied);
        }
        // 目前只实现"设置大小"（截断）——客户端上传覆盖时用得上。
        if let Some(size) = attrs.size {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&local)
                .map_err(|e| io_status(&e))?;
            file.set_len(size).map_err(|e| io_status(&e))?;
        }
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let Some(open) = self.files.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        if !open.writable {
            return Err(StatusCode::PermissionDenied);
        }
        if let Some(size) = attrs.size {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&open.path)
                .map_err(|e| io_status(&e))?;
            file.set_len(size).map_err(|e| io_status(&e))?;
        }
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let entries: Vec<File> = match self.fs.resolve(&path) {
            Ok(Resolved::Root) => self
                .fs
                .shares()
                .iter()
                .map(|share| {
                    let meta = fs::metadata(&share.local_path).ok();
                    let attrs = meta
                        .as_ref()
                        .map(attrs_of)
                        .unwrap_or_else(|| FileAttributes {
                            permissions: Some(0o040755),
                            ..FileAttributes::default()
                        });
                    File::new(share.name.clone(), attrs)
                })
                .collect(),
            Ok(Resolved::Inside { local_path, .. }) => {
                let meta = fs::metadata(&local_path).map_err(|e| io_status(&e))?;
                if !meta.is_dir() {
                    return Err(StatusCode::Failure);
                }
                let mut entries = Vec::new();
                for item in fs::read_dir(&local_path).map_err(|e| io_status(&e))? {
                    let item = item.map_err(|e| io_status(&e))?;
                    let name = item.file_name().to_string_lossy().into_owned();
                    let meta = item.metadata().map_err(|e| io_status(&e))?;
                    entries.push(dir_entry(&name, &meta));
                }
                entries.sort_by(|a, b| a.filename.cmp(&b.filename));
                entries
            }
            Err(_) => return Err(StatusCode::NoSuchFile),
        };

        let handle = self.new_handle("d");
        self.dirs
            .insert(handle.clone(), DirCursor { entries, index: 0 });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(cursor) = self.dirs.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if cursor.index >= cursor.entries.len() {
            return Err(StatusCode::Eof);
        }
        let end = (cursor.index + READDIR_BATCH).min(cursor.entries.len());
        let files: Vec<File> = cursor.entries[cursor.index..end].to_vec();
        cursor.index = end;
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let (local, share) = self.resolve_with_share(&filename)?;
        if self.is_read_only(share) {
            return Err(StatusCode::PermissionDenied);
        }
        let meta = fs::symlink_metadata(&local).map_err(|e| io_status(&e))?;
        if meta.is_dir() {
            // SFTP 里删目录要走 rmdir。
            return Err(StatusCode::Failure);
        }
        fs::remove_file(&local).map_err(|e| io_status(&e))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let (local, share) = self.resolve_with_share(&path)?;
        if self.is_read_only(share) {
            return Err(StatusCode::PermissionDenied);
        }
        fs::create_dir(&local).map_err(|e| io_status(&e))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let (local, share) = self.resolve_with_share(&path)?;
        if self.is_read_only(share) {
            return Err(StatusCode::PermissionDenied);
        }
        fs::remove_dir(&local).map_err(|e| io_status(&e))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let client = match self.fs.resolve(&path) {
            Ok(Resolved::Root) => "/".to_owned(),
            Ok(Resolved::Inside { share, local_path }) => {
                self.fs.to_client_path(&local_path).unwrap_or_else(|| {
                    let name = self
                        .fs
                        .share(share)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    format!("/{name}")
                })
            }
            Err(_) => return Err(StatusCode::NoSuchFile),
        };
        Ok(Name {
            id,
            files: vec![File::dummy(client)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let local = self.resolve_local(&path)?;
        let meta = fs::metadata(&local).map_err(|e| io_status(&e))?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let (old_local, old_share) = self.resolve_with_share(&oldpath)?;
        let (new_local, new_share) = self.resolve_with_share(&newpath)?;
        if self.is_read_only(old_share) || self.is_read_only(new_share) {
            return Err(StatusCode::PermissionDenied);
        }
        fs::rename(&old_local, &new_local).map_err(|e| io_status(&e))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let local = self.resolve_local(&path)?;
        let target = fs::read_link(&local).map_err(|e| io_status(&e))?;
        let text = target.to_string_lossy().into_owned();
        Ok(Name {
            id,
            files: vec![File::dummy(text)],
        })
    }

    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        let (link_local, share) = self.resolve_with_share(&linkpath)?;
        if self.is_read_only(share) {
            return Err(StatusCode::PermissionDenied);
        }
        create_symlink(&link_local, &targetpath).map_err(|e| io_status(&e))?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".to_owned(),
            language_tag: "en-US".to_owned(),
        })
    }
}
