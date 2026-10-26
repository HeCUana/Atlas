//! 集成测试共用的辅助设施。
//!
//! ## 为什么让测试二进制自己扮演子进程
//!
//! 进程托管必须用**真实子进程**验证，但不能依赖某个特定平台上的脚本解释器
//! 行为差异（`cmd /c` 与 `sh -c` 的行为并不等价）。这里不额外编译辅助二进制，
//! 而是让**测试二进制自己**再跑一次：父测试用
//! `--exact common::child_helper --nocapture` 只运行辅助用例，并用环境变量
//! `ATLAS_TEST_HELPER_MODE` 指定它的行为。
//!
//! 辅助子进程的三种行为：
//!
//! - [`MODE_SERVE`]：模拟 Minecraft 控制台——回显 stdin 的每一行，收到 `stop`
//!   时优雅退出，收到 `stderr` 时往 stderr 写一行；
//! - [`MODE_EXIT`]：立即以指定退出码退出（验证 `wait()` 的退出码）；
//! - [`MODE_FLOOD`]：往 stdout / stderr 各写约 400KB，验证两侧都被并发读取
//!   （只读一侧时子进程会卡在写满的管道上）。

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::BufRead;

use atlas_core::ServerConfig;

/// 辅助子进程模式：模拟控制台。
pub const MODE_SERVE: &str = "serve";
/// 辅助子进程模式：立即退出。
pub const MODE_EXIT: &str = "exit";
/// 辅助子进程模式：双侧大量输出。
pub const MODE_FLOOD: &str = "flood";
/// 辅助子进程模式：连续输出大量行（验证回放缓冲的容量上限）。
pub const MODE_BULK: &str = "bulk";

/// `MODE_BULK` 产出的行数：必须大于 [`atlas_server::BACKLOG_CAPACITY`]，
/// 才能验证"超出容量的早期行被淘汰"。
pub const BULK_LINES: usize = 800;

/// 指定辅助子进程行为的环境变量。
const ENV_MODE: &str = "ATLAS_TEST_HELPER_MODE";
/// 指定辅助子进程退出码的环境变量。
const ENV_EXIT_CODE: &str = "ATLAS_TEST_HELPER_EXIT_CODE";

/// 辅助用例在测试二进制里的完整名字（`--exact` 需要全名）。
const HELPER_TEST_NAME: &str = "common::child_helper";

/// 构造一个指向辅助子进程的服务器配置。
pub fn helper_config(id: &str, mode: &str, exit_code: i32) -> ServerConfig {
    let exe = std::env::current_exe().expect("无法定位当前测试可执行文件");
    let mut env = BTreeMap::new();
    env.insert(ENV_MODE.to_owned(), mode.to_owned());
    env.insert(ENV_EXIT_CODE.to_owned(), exit_code.to_string());
    ServerConfig {
        id: id.to_owned(),
        name: id.to_owned(),
        workdir: std::env::temp_dir(),
        command: exe.to_string_lossy().into_owned(),
        args: vec![
            "--exact".to_owned(),
            HELPER_TEST_NAME.to_owned(),
            "--nocapture".to_owned(),
        ],
        env,
        jvm_args: Vec::new(),
        auto_start: true,
        log_path: None,
    }
}

/// 辅助子进程：由父测试以子进程身份再次运行时才会真正执行。
#[test]
fn child_helper() {
    let Ok(mode) = std::env::var(ENV_MODE) else {
        // 父进程自身的测试运行：不做任何事情。
        return;
    };
    match mode.as_str() {
        MODE_EXIT => {
            let code = std::env::var(ENV_EXIT_CODE)
                .ok()
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0);
            std::process::exit(code);
        }
        MODE_SERVE => serve(),
        MODE_FLOOD => flood(),
        MODE_BULK => bulk(),
        _ => {}
    }
}

/// 模拟 Minecraft 控制台：回显 stdin，收到 `stop` 时退出。
fn serve() -> ! {
    // 行首补一个换行：测试框架会在同一行上打印 "test ... " 前缀。
    println!("\nATLAS-TEST-STDOUT-READY");
    eprintln!("ATLAS-TEST-STDERR-READY");

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    // 单独一个线程读 stdin，主线程负责产出输出。
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(line) => {
                let text = line.trim();
                match text {
                    "stop" => {
                        println!("\nATLAS-TEST-STOPPED");
                        std::process::exit(0);
                    }
                    "stderr" => {
                        eprintln!("ATLAS-TEST-STDERR-LINE");
                    }
                    _ => {
                        println!("\nATLAS-TEST-ECHO:{text}");
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // stdin 已关闭（父进程退出或管道断开）。
                std::process::exit(0);
            }
        }
    }
}

/// 往 stdout / stderr 各写约 400KB：管道缓冲区远小于此，只读一侧必然卡死。
fn flood() -> ! {
    const LINES: usize = 100;
    const PADDING: usize = 4096;
    let padding = "x".repeat(PADDING);
    for i in 0..LINES {
        println!("ATLAS-TEST-FLOOD-OUT-{i}-{padding}");
        eprintln!("ATLAS-TEST-FLOOD-ERR-{i}-{padding}");
    }
    println!("ATLAS-TEST-FLOOD-OUT-DONE");
    eprintln!("ATLAS-TEST-FLOOD-ERR-DONE");
    std::process::exit(0);
}

/// 连续输出 [`BULK_LINES`] 行：用于验证回放缓冲的容量上限与淘汰策略。
fn bulk() -> ! {
    for i in 0..BULK_LINES {
        println!("ATLAS-TEST-BULK-{i}-END");
    }
    println!("ATLAS-TEST-BULK-DONE");
    std::process::exit(0);
}
