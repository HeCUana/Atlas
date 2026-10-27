//! SFTP 交互式会话。
//!
//! `atlas sftp` 不带子命令时进入本会话，像 `sftp` 命令那样逐条执行；
//! 带子命令时（`atlas sftp ls /`）走一次性模式，便于脚本化。
//!
//! ## 路径约定
//!
//! 会话维护一个**根目录相对**的当前目录 `cwd`（空串表示根）。这是因为
//! SFTP 客户端的路径解析是 **chroot 语义**：传入路径的前导 `/` 会被忽略，
//! 永远被解释为根目录之下的相对路径。用户输入的相对路径在这里先与 `cwd`
//! 合并，再交给客户端，从而与 `ls`/`cd` 的直觉一致。

use std::path::PathBuf;

use atlas_sftp::{RemoteEntry, SftpClient};

/// 会话命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 列出目录。
    Ls {
        /// 路径（`None` 表示当前目录）。
        path: Option<String>,
    },
    /// 查看文件或目录信息。
    Stat {
        /// 路径。
        path: String,
    },
    /// 切换远端当前目录。
    Cd {
        /// 目标目录。
        path: String,
    },
    /// 显示远端当前目录。
    Pwd,
    /// 切换本地当前目录。
    Lcd {
        /// 目标目录。
        path: String,
    },
    /// 显示本地当前目录。
    Lpwd,
    /// 下载文件。
    Get {
        /// 远端路径。
        remote: String,
        /// 本地路径（`None` 表示用远端文件名）。
        local: Option<PathBuf>,
    },
    /// 上传文件。
    Put {
        /// 本地路径。
        local: PathBuf,
        /// 远端路径（`None` 表示用本地文件名放进当前目录）。
        remote: Option<String>,
    },
    /// 删除文件。
    Rm {
        /// 远端路径。
        path: String,
    },
    /// 删除目录（必须为空）。
    Rmdir {
        /// 远端路径。
        path: String,
    },
    /// 创建目录。
    Mkdir {
        /// 远端路径。
        path: String,
    },
    /// 执行本地 shell 命令（`!cmd` 前缀，与 OpenSSH 的 sftp 一致）。
    LocalShell {
        /// 要执行的命令。
        command: String,
    },
    /// 打印帮助。
    Help,
    /// 退出会话。
    Quit,
    /// 空行（忽略）。
    Empty,
}

/// 解析一行输入。
///
/// 返回 `None` 表示该行无法识别（调用方应给出错误提示，而不是退出）。
pub fn parse_command(line: &str) -> Option<Command> {
    // 去掉行首的 UTF-8 BOM：Windows PowerShell 5.1 把字符串管道给原生程序时
    // 会在开头加 BOM，导致第一条命令的动词变成 "\u{feff}ls" 而无法识别。
    let line = line.trim().trim_start_matches('\u{feff}').trim();
    if line.is_empty() {
        return Some(Command::Empty);
    }
    // `!` 前缀直接执行本地 shell 命令（与 OpenSSH 的 sftp 一致）。
    if let Some(rest) = line.strip_prefix('!') {
        let command = rest.trim();
        if command.is_empty() {
            return None;
        }
        return Some(Command::LocalShell {
            command: command.to_owned(),
        });
    }

    let mut parts = line.split_whitespace();
    let verb = parts.next()?.to_ascii_lowercase();
    let args: Vec<&str> = parts.collect();

    let need = |n: usize| -> Option<()> { if args.len() >= n { Some(()) } else { None } };

    Some(match verb.as_str() {
        "ls" | "dir" => {
            need(0)?;
            Command::Ls {
                path: args.first().map(|s| (*s).to_owned()),
            }
        }
        "ll" => {
            need(0)?;
            Command::Ls {
                path: args.first().map(|s| (*s).to_owned()),
            }
        }
        "stat" => {
            need(1)?;
            Command::Stat {
                path: args[0].to_owned(),
            }
        }
        "cd" => {
            need(1)?;
            Command::Cd {
                path: args[0].to_owned(),
            }
        }
        "pwd" => Command::Pwd,
        "lcd" => {
            need(1)?;
            Command::Lcd {
                path: args[0].to_owned(),
            }
        }
        "lpwd" => Command::Lpwd,
        "get" | "download" => {
            need(1)?;
            Command::Get {
                remote: args[0].to_owned(),
                local: args.get(1).map(PathBuf::from),
            }
        }
        "put" | "upload" => {
            need(1)?;
            Command::Put {
                local: PathBuf::from(args[0]),
                remote: args.get(1).map(|s| (*s).to_owned()),
            }
        }
        "rm" | "del" => {
            need(1)?;
            Command::Rm {
                path: args[0].to_owned(),
            }
        }
        "rmdir" => {
            need(1)?;
            Command::Rmdir {
                path: args[0].to_owned(),
            }
        }
        "mkdir" => {
            need(1)?;
            Command::Mkdir {
                path: args[0].to_owned(),
            }
        }
        "help" | "?" | "h" => Command::Help,
        "quit" | "exit" | "bye" | "q" => Command::Quit,
        _ => return None,
    })
}

/// 把用户输入的路径与当前目录合并，返回**根目录相对**路径。
///
/// - 绝对输入（以 `/` 开头）原样返回（交给客户端按 chroot 语义处理）；
/// - `..` 回退一级，但不能越过根；
/// - 其它相对路径拼接在 `cwd` 之后。
pub fn resolve_in_cwd(cwd: &str, input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(cwd.to_owned());
    }
    if input.starts_with('/') {
        return Ok(input.trim_start_matches('/').to_owned());
    }

    let mut stack: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    for seg in input.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return Err(format!("{input} 越出根目录"));
                }
            }
            other => stack.push(other),
        }
    }
    Ok(stack.join("/"))
}

/// 交互式会话状态。
pub struct Session {
    client: SftpClient,
    /// 远端当前目录（根目录相对）。
    cwd: String,
    /// 本地当前目录。
    local_cwd: PathBuf,
    /// 远端根目录（仅用于显示）。
    root: String,
}

impl Session {
    /// 创建会话。
    pub fn new(client: SftpClient, root: String) -> Self {
        Self {
            client,
            cwd: String::new(),
            local_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            root,
        }
    }

    /// 执行一条命令，返回 `false` 表示应退出会话。
    pub async fn execute(&mut self, command: Command) -> bool {
        match command {
            Command::Empty => {}
            Command::Quit => return false,
            Command::Help => print_help(),
            Command::Pwd => {
                let shown = if self.cwd.is_empty() {
                    "/".to_owned()
                } else {
                    format!("/{}", self.cwd)
                };
                println!("远端当前目录: {shown}");
            }
            Command::Lpwd => {
                println!("本地当前目录: {}", self.local_cwd.display());
            }
            Command::Ls { path } => {
                let target = match path {
                    Some(p) => match resolve_in_cwd(&self.cwd, &p) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("错误: {e}");
                            return true;
                        }
                    },
                    None => self.cwd.clone(),
                };
                match self.client.list(&target).await {
                    Ok(entries) => print_entries(&entries),
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Stat { path } => {
                let target = match resolve_in_cwd(&self.cwd, &path) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("错误: {e}");
                        return true;
                    }
                };
                match self.client.stat(&target).await {
                    Ok(entry) => {
                        println!("路径: {}", entry.path);
                        println!("类型: {}", if entry.is_dir { "目录" } else { "文件" });
                        println!("大小: {} 字节", entry.size);
                        if let Some(m) = entry.modified {
                            println!("修改时间: {m}");
                        }
                    }
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Cd { path } => {
                match resolve_in_cwd(&self.cwd, &path) {
                    Ok(target) => {
                        // 先确认确实是目录，再切换。
                        match self.client.stat(&target).await {
                            Ok(entry) if entry.is_dir => self.cwd = target,
                            Ok(_) => eprintln!("错误: {path} 不是目录"),
                            Err(e) => eprintln!("错误: {e}"),
                        }
                    }
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Lcd { path } => {
                let target = self.local_cwd.join(&path);
                if target.is_dir() {
                    self.local_cwd = target;
                } else {
                    eprintln!("错误: {} 不是本地目录", target.display());
                }
            }
            Command::Get { remote, local } => {
                let target = match resolve_in_cwd(&self.cwd, &remote) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("错误: {e}");
                        return true;
                    }
                };
                let local = local.unwrap_or_else(|| self.local_cwd.join(file_name_of(&remote)));
                match self.client.download(&target, &local).await {
                    Ok(()) => println!("已下载 {target} → {}", local.display()),
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Put { local, remote } => {
                let source = if local.is_absolute() {
                    local.clone()
                } else {
                    self.local_cwd.join(&local)
                };
                let target = match remote {
                    Some(r) => match resolve_in_cwd(&self.cwd, &r) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("错误: {e}");
                            return true;
                        }
                    },
                    None => {
                        let name = file_name_of(&source.to_string_lossy());
                        resolve_in_cwd(&self.cwd, &name).unwrap_or_else(|_| name.clone())
                    }
                };
                match self.client.upload(&source, &target).await {
                    Ok(()) => println!("已上传 {} → {target}", source.display()),
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Rm { path } => {
                let target = match resolve_in_cwd(&self.cwd, &path) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("错误: {e}");
                        return true;
                    }
                };
                match self.client.remove(&target).await {
                    Ok(()) => println!("已删除 {target}"),
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Rmdir { path } => {
                let target = match resolve_in_cwd(&self.cwd, &path) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("错误: {e}");
                        return true;
                    }
                };
                match self.client.remove_dir(&target).await {
                    Ok(()) => println!("已删除目录 {target}"),
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::Mkdir { path } => {
                let target = match resolve_in_cwd(&self.cwd, &path) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("错误: {e}");
                        return true;
                    }
                };
                match self.client.mkdir(&target).await {
                    Ok(()) => println!("已创建目录 {target}"),
                    Err(e) => eprintln!("错误: {e}"),
                }
            }
            Command::LocalShell { command } => {
                // 本地命令在子进程里跑，不阻塞会话的事件循环。
                let mut proc =
                    tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
                if cfg!(windows) {
                    proc.args(["/C", &command]);
                } else {
                    proc.args(["-c", &command]);
                }
                match proc.status().await {
                    Ok(status) => {
                        if let Some(code) = status.code()
                            && code != 0
                        {
                            eprintln!("（命令退出码 {code}）");
                        }
                    }
                    Err(e) => eprintln!("执行本地命令失败: {e}"),
                }
            }
        }
        true
    }
}

/// 打印帮助。
fn print_help() {
    println!("可用命令：");
    println!("  ls [路径]              列出远端目录");
    println!("  stat <路径>            查看远端文件/目录信息");
    println!("  cd <路径>              切换远端当前目录");
    println!("  pwd                    显示远端当前目录");
    println!("  lcd <路径>             切换本地当前目录");
    println!("  lpwd                   显示本地当前目录");
    println!("  get <远端> [本地]      下载文件");
    println!("  put <本地> [远端]      上传文件");
    println!("  rm <远端>              删除远端文件");
    println!("  rmdir <远端>           删除远端目录（须为空）");
    println!("  mkdir <远端>           创建远端目录");
    println!("  !<命令>               执行本地 shell 命令");
    println!("  help                   显示本帮助");
    println!("  quit                   退出会话");
}

/// 打印目录列表。
fn print_entries(entries: &[RemoteEntry]) {
    if entries.is_empty() {
        println!("（空目录）");
        return;
    }
    for entry in entries {
        let kind = if entry.is_dir { "d" } else { "-" };
        let size = if entry.is_dir { 0 } else { entry.size };
        println!("{kind} {size:>12}  {}", entry.path);
    }
}

/// 取路径的最后一段（文件名）。
fn file_name_of(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_owned()
}

/// 运行交互式会话。
pub async fn run(mut session: Session) {
    use tokio::io::{AsyncBufReadExt, BufReader};

    println!(
        "Atlas SFTP 交互会话 — 远端根目录: {}",
        if session.root.is_empty() {
            "/"
        } else {
            &session.root
        }
    );
    println!("输入 help 查看可用命令，quit 退出。");

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    loop {
        let cwd = if session.cwd.is_empty() {
            "/".to_owned()
        } else {
            format!("/{}", session.cwd)
        };
        print!("sftp:{cwd}> ");
        use std::io::Write;
        let _ = std::io::stdout().flush();

        let Ok(Some(line)) = lines.next_line().await else {
            // 输入流结束（Ctrl+D）——视为退出。
            break;
        };

        let Some(command) = parse_command(&line) else {
            eprintln!("未知命令，输入 help 查看可用命令。");
            continue;
        };

        if !session.execute(command).await {
            break;
        }
    }

    println!("已退出会话。");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析基本命令() {
        assert_eq!(parse_command("ls"), Some(Command::Ls { path: None }));
        assert_eq!(
            parse_command("ls /C:/Users"),
            Some(Command::Ls {
                path: Some("/C:/Users".to_owned())
            })
        );
        assert_eq!(
            parse_command("cd /tmp"),
            Some(Command::Cd {
                path: "/tmp".to_owned()
            })
        );
        assert_eq!(parse_command("pwd"), Some(Command::Pwd));
        assert_eq!(parse_command("quit"), Some(Command::Quit));
        assert_eq!(parse_command(""), Some(Command::Empty));
        assert_eq!(parse_command("   "), Some(Command::Empty));
    }

    #[test]
    fn 首行的_utf8_bom_不应破坏命令识别() {
        // Windows PowerShell 5.1 管道给原生程序时会在开头加 BOM。
        assert_eq!(
            parse_command("\u{feff}ls /"),
            Some(Command::Ls {
                path: Some("/".to_owned())
            })
        );
    }

    #[test]
    fn 未知命令返回空() {
        assert_eq!(parse_command("frobnicate"), None);
        assert_eq!(parse_command("ls"), Some(Command::Ls { path: None }));
    }

    #[test]
    fn 缺参数的命令被拒绝() {
        assert_eq!(parse_command("cd"), None);
        assert_eq!(parse_command("get"), None);
        assert_eq!(parse_command("put"), None);
        assert_eq!(parse_command("stat"), None);
    }

    #[test]
    fn 感叹号前缀执行本地命令() {
        assert_eq!(
            parse_command("!dir"),
            Some(Command::LocalShell {
                command: "dir".to_owned()
            })
        );
        assert_eq!(parse_command("!"), None);
    }

    #[test]
    fn 相对路径与当前目录合并() {
        assert_eq!(resolve_in_cwd("", "a/b").unwrap(), "a/b");
        assert_eq!(resolve_in_cwd("a", "b").unwrap(), "a/b");
        assert_eq!(resolve_in_cwd("a/b", "c").unwrap(), "a/b/c");
        assert_eq!(resolve_in_cwd("a/b", "..").unwrap(), "a");
        assert_eq!(resolve_in_cwd("a", "./b").unwrap(), "a/b");
    }

    #[test]
    fn 绝对路径不被当前目录改写() {
        assert_eq!(resolve_in_cwd("a/b", "/c/d").unwrap(), "c/d");
    }

    #[test]
    fn 上级目录不能越过根() {
        assert!(resolve_in_cwd("", "..").is_err());
        assert!(resolve_in_cwd("a", "../..").is_err());
    }

    #[test]
    fn 取路径最后一段作为文件名() {
        assert_eq!(file_name_of("a/b/c.txt"), "c.txt");
        assert_eq!(file_name_of("c.txt"), "c.txt");
        assert_eq!(file_name_of("a/b/"), "b");
    }
}
