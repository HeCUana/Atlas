//! 远程根目录语义测试。
//!
//! 覆盖两条主线：
//! - [`to_absolute_remote`]：把用户写的根目录（可能是 Windows 反斜杠路径）
//!   转成 SFTP 的绝对 POSIX 形式；
//! - [`resolve_under_root`]：用户路径解析到绝对路径，且**不得越出根目录**。

use atlas_sftp::path::{resolve_under_root, to_absolute_remote};

#[test]
fn windows_反斜杠路径被转成_sftp_绝对形式() {
    assert_eq!(
        to_absolute_remote("C:\\Users\\Administrator\\Desktop").unwrap(),
        "/C:/Users/Administrator/Desktop"
    );
}

#[test]
fn windows_正斜杠路径同样被转成_sftp_绝对形式() {
    assert_eq!(
        to_absolute_remote("C:/Users/Administrator/Desktop").unwrap(),
        "/C:/Users/Administrator/Desktop"
    );
}

#[test]
fn posix_路径保持不变() {
    assert_eq!(to_absolute_remote("/srv/mc").unwrap(), "/srv/mc");
    assert_eq!(to_absolute_remote("/srv/./mc/").unwrap(), "/srv/mc");
}

#[test]
fn 空路径与斜杠都表示文件系统根() {
    assert_eq!(to_absolute_remote("").unwrap(), "/");
    assert_eq!(to_absolute_remote("/").unwrap(), "/");
    assert_eq!(to_absolute_remote("   ").unwrap(), "/");
}

#[test]
fn 裸盘符表示该盘根目录() {
    // `C:` 在 SFTP 侧就是盘根（`ls /` 会看到 C: 与 D:），是合法路径。
    assert_eq!(to_absolute_remote("C:").unwrap(), "/C:");
    // 但 `C:foo` 是"盘内相对路径"，语义不明确，拒绝。
    assert!(to_absolute_remote("C:foo").is_err());
}

#[test]
fn 相对路径按文件系统根处理() {
    assert_eq!(to_absolute_remote("srv/mc").unwrap(), "/srv/mc");
}

#[test]
fn 相对输入拼在根目录之下() {
    assert_eq!(
        resolve_under_root("/srv/mc", "plugins").unwrap(),
        "/srv/mc/plugins"
    );
}

#[test]
fn 绝对输入必须在根目录之下() {
    // `/plugins` 是**绝对**路径，不在根 `/srv/mc` 之下 → 拒绝。
    // （相对语义请用不带前导斜杠的 `plugins`）
    assert!(resolve_under_root("/srv/mc", "/plugins").is_err());
    assert_eq!(
        resolve_under_root("/srv/mc", "plugins").unwrap(),
        "/srv/mc/plugins"
    );
}

#[test]
fn 根目录内的绝对路径原样保留() {
    assert_eq!(
        resolve_under_root("/srv/mc", "/srv/mc/x").unwrap(),
        "/srv/mc/x"
    );
    assert_eq!(resolve_under_root("/srv/mc", "/srv/mc").unwrap(), "/srv/mc");
}

#[test]
fn 根目录外的绝对路径被拒绝() {
    assert!(resolve_under_root("/srv/mc", "/etc/passwd").is_err());
    assert!(resolve_under_root("/srv/mc", "/srv/other").is_err());
}

#[test]
fn 空路径返回根目录本身() {
    assert_eq!(resolve_under_root("/srv/mc", "").unwrap(), "/srv/mc");
    assert_eq!(resolve_under_root("/", "").unwrap(), "/");
}

#[test]
fn 相对输入不能用_上跳_越过根目录() {
    assert!(resolve_under_root("/srv/mc", "../../etc").is_err());
    assert!(resolve_under_root("/srv/mc", "a/../../etc").is_err());
}

#[test]
fn windows_根目录下的解析() {
    let root = "C:\\Users\\Administrator\\Desktop";
    assert_eq!(
        resolve_under_root(root, "sub").unwrap(),
        "/C:/Users/Administrator/Desktop/sub"
    );
    assert_eq!(
        resolve_under_root(root, "/C:/Users/Administrator/Desktop/x").unwrap(),
        "/C:/Users/Administrator/Desktop/x"
    );
    assert!(resolve_under_root(root, "/C:/Windows").is_err());
}

#[test]
fn 根目录是文件系统根时任何绝对路径都在其下() {
    assert_eq!(resolve_under_root("/", "/etc").unwrap(), "/etc");
    assert_eq!(resolve_under_root("/", "etc").unwrap(), "/etc");
}
