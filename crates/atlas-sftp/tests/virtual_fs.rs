//! 虚拟文件系统的路径映射测试。
//!
//! 这是 SFTP 服务端最关键的安全边界：客户端给的路径必须被映射到
//! share 之内，且 `..` 不能越出。

use std::path::PathBuf;

use atlas_sftp::server::{Resolved, SftpShare, VirtualFs};

fn fs() -> VirtualFs {
    VirtualFs::with_shares(vec![
        SftpShare::new("survival", "servers/survival"),
        SftpShare::new("creative", "servers/creative"),
        SftpShare::new("readonly", "servers/ro").read_only(),
    ])
}

#[test]
fn 根路径解析为虚拟根() {
    let fs = fs();
    assert_eq!(fs.resolve("/").unwrap(), Resolved::Root);
    assert_eq!(fs.resolve("").unwrap(), Resolved::Root);
    assert_eq!(fs.resolve("///").unwrap(), Resolved::Root);
}

#[test]
fn 共享目录映射到真实路径() {
    let fs = fs();
    match fs.resolve("/survival").unwrap() {
        Resolved::Inside { share, local_path } => {
            assert_eq!(share, 0);
            assert_eq!(local_path, PathBuf::from("servers/survival"));
        }
        other => panic!("期望 Inside，实际 {other:?}"),
    }
}

#[test]
fn 共享目录下的子路径() {
    let fs = fs();
    match fs.resolve("/survival/plugins/x.jar").unwrap() {
        Resolved::Inside { share, local_path } => {
            assert_eq!(share, 0);
            assert_eq!(local_path, PathBuf::from("servers/survival/plugins/x.jar"));
        }
        other => panic!("期望 Inside，实际 {other:?}"),
    }
}

#[test]
fn 点目录被丢弃() {
    let fs = fs();
    match fs.resolve("/survival/./plugins/./x.jar").unwrap() {
        Resolved::Inside { local_path, .. } => {
            assert_eq!(local_path, PathBuf::from("servers/survival/plugins/x.jar"));
        }
        other => panic!("期望 Inside，实际 {other:?}"),
    }
}

#[test]
fn 上级目录在共享内可回退() {
    let fs = fs();
    match fs.resolve("/survival/plugins/..").unwrap() {
        Resolved::Inside { local_path, .. } => {
            assert_eq!(local_path, PathBuf::from("servers/survival"));
        }
        other => panic!("期望 Inside，实际 {other:?}"),
    }
}

#[test]
fn 上级目录不能越过虚拟根() {
    let fs = fs();
    assert!(fs.resolve("/..").is_err());
    assert!(fs.resolve("/survival/../..").is_err());
    assert!(fs.resolve("/survival/a/../../..").is_err());
}

#[test]
fn 未知共享目录被拒绝() {
    let fs = fs();
    let err = fs.resolve("/nonexistent").unwrap_err();
    assert!(
        err.to_string().contains("未知的共享目录"),
        "错误信息应说明是未知共享，实际：{err}"
    );
}

#[test]
fn 共享目录之间可以横向移动() {
    // `/survival/../creative` 等价于 `/creative`——合法，仍在根之内。
    let fs = fs();
    match fs.resolve("/survival/../creative").unwrap() {
        Resolved::Inside { share, local_path } => {
            assert_eq!(share, 1);
            assert_eq!(local_path, PathBuf::from("servers/creative"));
        }
        other => panic!("期望 Inside，实际 {other:?}"),
    }
}

#[test]
fn 反向映射回客户端路径() {
    let fs = fs();
    assert_eq!(
        fs.to_client_path(std::path::Path::new("servers/survival")),
        Some("/survival".to_owned())
    );
    assert_eq!(
        fs.to_client_path(std::path::Path::new("servers/survival/plugins")),
        Some("/survival/plugins".to_owned())
    );
    assert_eq!(fs.to_client_path(std::path::Path::new("其它地方")), None);
}

#[test]
fn 共享目录名不能重复或含分隔符() {
    let mut fs = VirtualFs::new();
    fs.add_share(SftpShare::new("a", "x")).unwrap();
    assert!(fs.add_share(SftpShare::new("a", "y")).is_err());
    assert!(fs.add_share(SftpShare::new("b/c", "z")).is_err());
    assert!(fs.add_share(SftpShare::new("  ", "z")).is_err());
}

#[test]
fn 只读标记被保留() {
    let fs = fs();
    assert!(!fs.share(0).unwrap().read_only);
    assert!(fs.share(2).unwrap().read_only);
}
