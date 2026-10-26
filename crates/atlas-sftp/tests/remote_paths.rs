//! 远程路径处理的集成测试。
//!
//! 覆盖路径拼接、规整以及**本地路径穿越防护**等安全场景。
//! 这些测试全部是纯函数调用，不需要真实的 SSH 服务器。

use std::path::{Path, PathBuf};

use atlas_sftp::{human_size, is_hidden, join_remote, normalize_remote, sanitize_local_path};

/// 统一的本地根目录。
fn root() -> PathBuf {
    PathBuf::from("/var/lib/atlas")
}

#[test]
fn join_remote_builds_absolute_path_under_root() {
    assert_eq!(
        join_remote("/srv/mc", "plugins").unwrap(),
        "/srv/mc/plugins"
    );
    assert_eq!(
        join_remote("/srv/mc", "plugins/config.yml").unwrap(),
        "/srv/mc/plugins/config.yml"
    );
}

#[test]
fn join_remote_ignores_leading_slash_of_relative_part() {
    // 前导 `/` 不应让路径跳出根目录。
    assert_eq!(
        join_remote("/srv/mc", "/plugins").unwrap(),
        "/srv/mc/plugins"
    );
    assert_eq!(join_remote("/srv/mc", "/").unwrap(), "/srv/mc");
}

#[test]
fn join_remote_handles_trailing_slash_in_base() {
    assert_eq!(
        join_remote("/srv/mc/", "plugins").unwrap(),
        "/srv/mc/plugins"
    );
    assert_eq!(
        join_remote("/srv/mc//", "plugins").unwrap(),
        "/srv/mc/plugins"
    );
}

#[test]
fn join_remote_rejects_escape_above_root() {
    for evil in [
        "../etc",
        "../../etc/passwd",
        "plugins/../../etc/passwd",
        "./../etc",
    ] {
        let err = join_remote("/srv/mc", evil).expect_err("越出根目录的路径必须被拒绝");
        assert!(
            err.to_string().contains("越出根目录"),
            "错误信息应说明越界: {err}"
        );
    }
}

#[test]
fn join_remote_allows_parent_inside_root() {
    assert_eq!(
        join_remote("/srv/mc", "plugins/../logs").unwrap(),
        "/srv/mc/logs"
    );
}

#[test]
fn join_remote_with_empty_base_keeps_relative_form() {
    assert_eq!(join_remote("", "plugins").unwrap(), "plugins");
    assert_eq!(join_remote("", "").unwrap(), ".");
}

#[test]
fn normalize_remote_canonicalizes_dot_segments() {
    assert_eq!(normalize_remote("a/./b").unwrap(), "a/b");
    assert_eq!(normalize_remote("./a/b/.").unwrap(), "a/b");
}

#[test]
fn normalize_remote_canonicalizes_parent_segments() {
    assert_eq!(normalize_remote("a/b/../c").unwrap(), "a/c");
    assert_eq!(normalize_remote("a/b/c/../../d").unwrap(), "a/d");
}

#[test]
fn normalize_remote_keeps_absolute_paths_absolute() {
    assert_eq!(normalize_remote("/srv/./mc/").unwrap(), "/srv/mc");
    assert_eq!(normalize_remote("/").unwrap(), "/");
}

#[test]
fn normalize_remote_rejects_escape_above_root() {
    assert!(normalize_remote("../etc").is_err());
    assert!(normalize_remote("/../etc").is_err());
    assert!(normalize_remote("a/../../etc").is_err());
}

#[test]
fn sanitize_local_path_maps_relative_path_into_root() {
    let mapped = sanitize_local_path(&root(), "plugins/config.yml").unwrap();
    assert_eq!(mapped, Path::new("/var/lib/atlas/plugins/config.yml"));
    assert!(mapped.starts_with(root()));
}

#[test]
fn sanitize_local_path_blocks_parent_traversal() {
    for evil in [
        "../../etc/passwd",
        "plugins/../../etc/passwd",
        "..",
        "../",
        "a/b/../../../c",
    ] {
        let err = sanitize_local_path(&root(), evil).expect_err("含 .. 的路径必须被拒绝");
        assert!(
            err.to_string().contains("越界"),
            "错误信息应说明越界: {err}"
        );
    }
}

#[test]
fn sanitize_local_path_blocks_windows_style_traversal() {
    // Windows 上的 `..\` 与盘符同样要拦住。
    assert!(sanitize_local_path(&root(), "..\\..\\windows\\system32").is_err());
    assert!(sanitize_local_path(&root(), "plugins\\..\\..\\etc").is_err());
    assert!(sanitize_local_path(&root(), "C:\\windows\\system32").is_err());
    assert!(sanitize_local_path(&root(), "plugins\\config.yml").is_ok());
}

#[test]
fn sanitize_local_path_rejects_absolute_input() {
    assert!(sanitize_local_path(&root(), "/etc/passwd").is_err());
    assert!(sanitize_local_path(&root(), "\\windows").is_err());
}

#[test]
fn sanitize_local_path_never_escapes_root() {
    // 逐个断言：无论输入什么，只要成功，结果必须在 root 之下。
    let candidates = ["a", "a/b/c", "./a/./b", "weird name/文件.txt"];
    for candidate in candidates {
        let mapped = sanitize_local_path(&root(), candidate)
            .unwrap_or_else(|e| panic!("{candidate} 应当被接受: {e}"));
        assert!(
            mapped.starts_with(root()),
            "{candidate} 映射到了根目录之外: {mapped:?}"
        );
    }
}

#[test]
fn sanitize_local_path_rejects_parent_even_when_it_stays_inside() {
    // 策略是「见到 .. 就拒绝」，不做归一化：宁可报错，也不猜测调用方的意图。
    assert!(sanitize_local_path(&root(), "a/b/../c").is_err());
    assert!(sanitize_local_path(&root(), "a/./b").is_ok());
}

#[test]
fn sanitize_local_path_empty_relative_maps_to_root() {
    assert_eq!(sanitize_local_path(&root(), "").unwrap(), root());
}

#[test]
fn is_hidden_marks_dot_files() {
    assert!(is_hidden(".env"));
    assert!(is_hidden(".git"));
    assert!(!is_hidden("server.jar"));
    assert!(!is_hidden("world"));
}

#[test]
fn human_size_renders_common_units() {
    assert_eq!(human_size(0), "0 B");
    assert_eq!(human_size(999), "999 B");
    assert_eq!(human_size(1024), "1.0 KB");
    assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
    assert_eq!(human_size(1024u64.pow(3)), "1.0 GB");
}
