//! Atlas 子进程托管。
//!
//! 负责把 Minecraft 服务端作为**子进程**启动，并提供：
//!
//! - 进程生命周期（开服 / 关服 / 重启）
//! - 控制台输出行的流式分发
//! - 终端命令执行与返回信息采集
//!
//! ## 关键约束
//!
//! - stdout 与 stderr **必须由两个独立任务并发读取**，否则高日志量下管道写满
//!   会造成互相阻塞死锁。
//! - 行分隔不能只认 `\n`：Minecraft 控制台的进度条用 `\r` 覆盖同一行，
//!   `\r` 之间的片段属于"同一行的进度更新"，不是独立日志行。
//! - 子进程退出必须与 Atlas 主流程解耦，Atlas 退出前要先让子进程优雅退出。

pub mod console;
pub mod process;
pub mod reader;
pub mod supervisor;

pub use console::{ConsoleBridge, ConsoleSource};
pub use process::{BACKLOG_CAPACITY, ManagedServer};
pub use reader::LineSplitter;
pub use supervisor::Supervisor;
