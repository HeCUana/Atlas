//! Atlas 命令行入口。
//!
//! 子命令：
//! - `start` / `stop` / `restart`：批量开关服
//! - `logs`：查看某台服务器的日志
//! - `tui`：进入终端界面
//! - `plugin`：插件管理
//! - `config`：配置检查
//!
//! ## 数据流
//!
//! ```text
//! 子进程 stdout/stderr
//!        │  atlas-server::LineSplitter 切行
//!        ▼
//!   atlas-server::Supervisor 广播原始行
//!        │  atlas_parser::LogParser 解析
//!        ▼
//!   atlas_core::EventBus 广播结构化事件
//!        │
//!        ├──▶ atlas-tui 渲染
//!        └──▶ 插件订阅
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use atlas_core::{AtlasConfig, Event, EventBus, LogLevel, ServerId};
use atlas_parser::LogParser;
use atlas_server::{ConsoleBridge, Supervisor};
use clap::{Parser, Subcommand};
use russh::server::Server as _;

mod remote;
mod scaffold;
mod sshd;

#[derive(Parser, Debug)]
#[command(name = "atlas", version, about = "Atlas — Minecraft 多服务器管理工具")]
struct Cli {
    /// 配置文件路径。
    #[arg(short, long, default_value = "atlas.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 启动服务器。
    Start {
        /// 不指定则启动所有标记了 auto_start 的服务器。
        #[arg(value_name = "SERVER")]
        servers: Vec<String>,
    },
    /// 停止服务器。
    Stop {
        /// 不指定则停止全部服务器。
        #[arg(value_name = "SERVER")]
        servers: Vec<String>,
    },
    /// 重启服务器。
    Restart {
        /// 不指定则重启全部服务器。
        #[arg(value_name = "SERVER")]
        servers: Vec<String>,
    },
    /// 查看服务器日志。
    Logs {
        /// 服务器 ID。
        server: String,
        /// 持续跟随输出。
        #[arg(short, long)]
        follow: bool,
        /// 只显示该级别及以上的日志。
        #[arg(long)]
        level: Option<String>,
    },
    /// 进入终端界面。
    Tui,
    /// 插件管理。
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// 检查配置文件。
    Config,
    /// 生成配置文件与必要目录。
    Init {
        /// 覆盖已存在的配置文件。
        #[arg(long)]
        force: bool,
    },
    /// 生成 SSH 密钥对（服务端主机密钥 / 第三方客户端凭据）。
    Keygen {
        /// 私钥输出路径（默认按格式取 keys/id_ed25519 或 keys/id_ed25519.pem）。
        #[arg(long)]
        path: Option<PathBuf>,
        /// 私钥格式：openssh（默认，ssh-keygen 产物）或 pkcs8（标准 PEM）。
        #[arg(long, default_value = "openssh")]
        format: String,
        /// 私钥口令（仅 pkcs8 格式支持加密）。
        #[arg(long)]
        passphrase: Option<String>,
        /// 覆盖已存在的私钥（会销毁旧凭据）。
        #[arg(long)]
        force: bool,
        /// 公钥注释，便于在 authorized_keys 里辨认来源。
        #[arg(long, default_value = "atlas")]
        comment: String,
    },
    /// 管理本机 SSH 服务端（sshd）——让第三方 SFTP 客户端能连本机。
    Sshd {
        #[command(subcommand)]
        action: SshdAction,
    },
    /// 启动 SFTP 服务端，让远端电脑通过 SFTP 管理 Atlas 托管的服务器文件。
    Serve,
    /// 启动远程 TUI：远端电脑用 SSH 连进来操作本进程托管的服务器。
    Remote,
}

#[derive(Subcommand, Debug)]
enum SshdAction {
    /// 查询服务状态与 SFTP 就绪情况。
    Status,
    /// 启动服务（并设为开机自启）。
    Start,
    /// 停止服务。
    Stop,
    /// 安装 SSH 服务端。
    Install,
    /// 把 Atlas 客户端公钥装进服务端的 authorized_keys。
    Authorize,
}

#[derive(Subcommand, Debug)]
enum PluginAction {
    /// 列出插件。
    List,
    /// 启用插件。
    Enable { id: String },
    /// 禁用插件。
    Disable { id: String },
    /// 热重载插件。
    Reload { id: String },
}

/// Atlas 运行期上下文。
struct Atlas {
    config: AtlasConfig,
    bus: EventBus,
    supervisor: Supervisor,
    parser: LogParser,
}

impl Atlas {
    fn new(config: AtlasConfig) -> Self {
        let bus = EventBus::new(config.event_capacity);
        let supervisor = Supervisor::new(config.clone(), bus.clone());

        // 日志解析 profile：内置两套 + 可选的配置覆盖。
        let mut parser = LogParser::new();
        if let Some(path) = &config.log_profiles {
            if let Err(e) = parser.load_profile_overrides(path) {
                tracing::warn!("加载日志 profile 覆盖 {} 失败: {e}", path.display());
            } else {
                tracing::info!("已加载日志 profile 覆盖: {}", path.display());
            }
        }

        Self {
            config,
            bus,
            supervisor,
            parser,
        }
    }

    /// 为每台服务器启动"原始行 → 结构化事件"的日志泵。
    ///
    /// `echo` 为真时，同时把原始行按 `[服务器ID] ` 前缀打印到标准输出——
    /// 无界面模式下这是运维唯一能看到的运行状态。
    fn spawn_log_pumps(&self, echo: bool) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        for id in self.config.server_ids() {
            let Some(mut rx) = self.supervisor.subscribe_lines(id.as_str()) else {
                continue;
            };
            let parser = self.parser.clone();
            let bus = self.bus.clone();
            let server_id = id.clone();
            handles.push(tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(line) => {
                            if echo {
                                println!("[{server_id}] {line}");
                            }
                            let event = parser.parse(&server_id, &line);
                            bus.publish(Event::Log(event));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("{} 的日志流落后 {n} 行", server_id);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }));
        }
        handles
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;

    runtime.block_on(async_run(cli))
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("atlas=info,atlas_core=info,atlas_parser=info,atlas_server=info")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn async_run(cli: Cli) -> Result<()> {
    // `init` 不要求配置已存在：文件在就沿用它，不在就用默认值去生成。
    let config = match &cli.command {
        Commands::Init { .. } => load_config_if_exists(&cli.config)?.unwrap_or_default(),
        _ => load_config(&cli.config)?,
    };

    let mut atlas = Atlas::new(config);

    match cli.command {
        Commands::Start { servers } => {
            let targets = resolve_targets(&atlas.config, servers, true);
            if targets.is_empty() {
                print_nothing_to_start(&atlas.config);
                return Ok(());
            }
            for id in &targets {
                atlas.supervisor.start(id).await?;
            }
            print_start_banner(&atlas.config, &targets);
            let _pumps = atlas.spawn_log_pumps(true);
            wait_for_shutdown(&mut atlas.supervisor).await;
        }
        Commands::Stop { servers } => {
            let targets = resolve_targets(&atlas.config, servers, false);
            if targets.is_empty() {
                println!("配置里没有服务器，无需停止。");
                return Ok(());
            }
            let mut failures = Vec::new();
            for id in &targets {
                match atlas.supervisor.stop(id).await {
                    Ok(()) => println!("已停止 {id}"),
                    Err(e) => {
                        eprintln!("停止 {id} 失败: {e}");
                        failures.push(id.clone());
                    }
                }
            }
            if !failures.is_empty() {
                anyhow::bail!("以下服务器未能停止: {}", failures.join(", "));
            }
        }
        Commands::Restart { servers } => {
            let targets = resolve_targets(&atlas.config, servers, false);
            if targets.is_empty() {
                print_nothing_to_start(&atlas.config);
                return Ok(());
            }
            for id in &targets {
                atlas.supervisor.restart(id).await?;
            }
            print_start_banner(&atlas.config, &targets);
            let _pumps = atlas.spawn_log_pumps(true);
            wait_for_shutdown(&mut atlas.supervisor).await;
        }
        Commands::Logs {
            server,
            follow,
            level,
        } => {
            let min = level.map(|l| LogLevel::parse(&l));
            atlas.supervisor.start(&server).await?;
            let mut rx = atlas
                .supervisor
                .subscribe_lines(&server)
                .ok_or_else(|| anyhow::anyhow!("无法订阅 {server} 的输出"))?;
            let server_id = ServerId::new(server.clone());
            loop {
                let Ok(line) = rx.recv().await else {
                    break;
                };
                let event = atlas.parser.parse(&server_id, &line);
                if let Some(min) = min
                    && event.level < min
                {
                    continue;
                }
                println!("{}", event.message);
                if !follow {
                    break;
                }
            }
        }
        Commands::Tui => {
            run_tui(&mut atlas).await?;
        }
        Commands::Plugin { action } => {
            let mut registry = atlas_plugin::PluginRegistry::new(atlas_plugin::PluginLoader::new(
                &atlas.config.plugin_dir,
            ));
            registry.refresh()?;
            match action {
                PluginAction::List => {
                    for (plugin, status) in registry.list() {
                        println!(
                            "{:<20} {:<12} {}",
                            plugin.manifest.id,
                            status.label(),
                            plugin.kind.label()
                        );
                    }
                }
                PluginAction::Enable { id } => {
                    registry.enable(&id)?;
                    println!("已启用 {id}");
                }
                PluginAction::Disable { id } => {
                    registry.disable(&id)?;
                    println!("已禁用 {id}");
                }
                PluginAction::Reload { id } => {
                    registry.reload(&id)?;
                    println!("已重载 {id}");
                }
            }
        }
        Commands::Config => {
            println!("配置校验通过，共 {} 台服务器", atlas.config.servers.len());
            for s in &atlas.config.servers {
                println!("  - {} ({})", s.id, s.display_name());
            }
            println!("插件目录: {}", atlas.config.plugin_dir.display());
            println!("事件容量: {}", atlas.config.event_capacity);
        }
        Commands::Init { force } => {
            let result = scaffold::scaffold(&cli.config, &atlas.config, force, true)?;
            println!("{}", result.summary());
            println!();
            println!(
                "下一步：编辑 {} 添加服务器，然后用 `atlas start` 启动。",
                cli.config.display()
            );
        }
        Commands::Keygen {
            path,
            format,
            passphrase,
            force,
            comment,
        } => {
            let format = atlas_sftp::KeyFormat::parse(&format).ok_or_else(|| {
                anyhow::anyhow!("未知的私钥格式: {format}（可选 openssh / pkcs8）")
            })?;
            let path = path.unwrap_or_else(|| {
                PathBuf::from(match format {
                    atlas_sftp::KeyFormat::Pkcs8 => AtlasConfig::DEFAULT_KEY_PATH_PKCS8,
                    atlas_sftp::KeyFormat::OpenSsh => AtlasConfig::DEFAULT_KEY_PATH,
                })
            });

            let pair = atlas_sftp::generate_ed25519_with(
                &path,
                Some(&comment),
                force,
                format,
                passphrase.as_deref(),
            )?;
            println!("已生成 SSH 密钥对：");
            println!("  私钥: {}", pair.private_path.display());
            println!("  公钥: {}", pair.public_path.display());
            println!("  格式: {}", pair.format.as_str());
            println!("  指纹: {}", pair.fingerprint);
            if pair.encrypted {
                println!("  加密: 是");
            }
            println!();
            println!("公钥内容（形如 `ssh-ed25519 AAAA... 备注`）：");
            println!("  {}", pair.public_key);
            println!();
            println!("这把密钥的两种用途：");
            println!("  1) 当 SFTP 服务端的主机密钥 —— atlas keygen --path keys/host_ed25519");
            println!("     远端客户端首次连接时校验的就是它；路径写在 [sftp_server] host_key。");
            println!("  2) 当第三方客户端连本机的凭据 —— atlas sshd authorize");
            println!("     会把对应的公钥装进本机 sshd 的 authorized_keys。");
            #[cfg(windows)]
            println!(
                "\n（Windows 提示：私钥文件继承所在目录的 ACL，请确认 {} 目录只对本人可写。）",
                pair.private_path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
        }
        Commands::Sshd { action } => match action {
            SshdAction::Status => {
                // 私钥路径固定用默认位置：Atlas 不再作为 SFTP 客户端，
                // 这里要检查的是「供第三方客户端连本机用的那把私钥」。
                let key_path = PathBuf::from(AtlasConfig::DEFAULT_KEY_PATH);
                let st = sshd::status(&key_path)?;
                println!("本机 SSH 服务端（sshd）状态");
                println!("  服务已安装    {}", mark(st.installed));
                println!("  服务正在运行  {}", mark(st.running));
                println!(
                    "  开机自启      {}",
                    st.startup.as_deref().unwrap_or("未知")
                );
                println!(
                    "  {} 端口监听   {}",
                    sshd::DEFAULT_SSH_PORT,
                    mark(st.port_listening)
                );
                println!("  客户端私钥    {}", mark(st.private_key_present));
                println!("  公钥已授权    {}", mark(st.public_key_installed));
                if let Some(p) = &st.public_key_path {
                    println!("    ↳ {}", p.display());
                }
                if st.public_key_file_stale {
                    println!("    ↳ ⚠ .pub 文件与私钥不匹配（陈旧），以私钥推导的公钥为准");
                }
                println!("  known_hosts   {}", mark(st.known_host_present));
                if let Some(fp) = &st.host_key_fingerprint {
                    println!("  主机密钥指纹  {fp}");
                    println!("    ↳ 第三方客户端首次连接弹出的指纹应与此一致");
                }
                // 机器可读的一行摘要，给脚本和 CI 断言用（三个平台后端不同，
                // 只有统一成 key=value 才能跨平台比对）。
                println!("{}", st.summary_line());
                println!();
                if st.authorized_keys_has_private_key {
                    println!("⛔ 严重：authorized_keys 里混有**私钥**内容。");
                    println!("   私钥出现在公钥位置既让认证失败，也等于凭据泄漏。");
                    println!("   请人工清理该文件；曾经写入的私钥应视为已泄漏，需作废并重新生成。");
                    println!();
                }
                if st.is_ready() {
                    println!("SFTP 就绪 ✓");
                } else {
                    println!("SFTP 尚未就绪，缺这几项：");
                    let mut blockers = 0usize;
                    if !st.installed {
                        println!("  · 未安装 SSH 服务端 → `atlas sshd install`");
                        blockers += 1;
                    }
                    if !st.running {
                        println!("  · 服务未运行 → `atlas sshd start`");
                        blockers += 1;
                    }
                    if !st.port_listening {
                        println!("  · 端口未监听 → 检查服务状态与防火墙");
                        blockers += 1;
                    }
                    if !st.private_key_present {
                        println!("  · 客户端私钥不存在 → `atlas keygen`");
                        blockers += 1;
                    }
                    if !st.public_key_installed {
                        println!("  · 公钥未装进服务端 → 运行 `atlas sshd authorize`");
                        blockers += 1;
                    }
                    if !st.known_host_present {
                        println!(
                            "  · 主机公钥不在 known_hosts → 首次连接用 `atlas sftp --accept-unknown-host`"
                        );
                        blockers += 1;
                    }
                    if blockers == 0 {
                        // 唯一的阻塞项是上面已提示过的私钥污染。
                        println!("  · authorized_keys 混有私钥内容 → 见上方说明");
                    }
                }
            }
            SshdAction::Start => {
                sshd::start()?;
                println!("sshd 已启动（并设为开机自启）。");
            }
            SshdAction::Stop => {
                sshd::stop()?;
                println!("sshd 已停止。");
            }
            SshdAction::Install => {
                sshd::install()?;
                println!("SSH 服务端已安装。接下来运行 `atlas sshd start` 启动它。");
            }
            SshdAction::Authorize => {
                let key_path = PathBuf::from(AtlasConfig::DEFAULT_KEY_PATH);
                let target = sshd::authorize(&key_path)?;
                println!("客户端公钥已装入 {}", target.display());
            }
        },
        Commands::Serve => {
            let cfg = atlas.config.sftp_server.clone();
            if !cfg.enabled {
                anyhow::bail!(
                    "配置里 [sftp_server] 还没启用。先编辑 {} 把 enabled 改成 true；\n\
                     （`atlas init` 可以给旧配置补上这一段）",
                    cli.config.display()
                );
            }

            // 共享目录：先从 [[servers]] 派生（id → workdir），
            // 再叠加 [[sftp_server.share]] 显式声明的。
            let mut shares = Vec::new();
            for server in &atlas.config.servers {
                shares.push(atlas_sftp::server::SftpShare::new(
                    &server.id,
                    &server.workdir,
                ));
            }
            for share in &cfg.shares {
                shares.push(atlas_sftp::server::SftpShare {
                    name: share.name.clone(),
                    local_path: share.path.clone(),
                    read_only: share.read_only,
                });
            }
            if shares.is_empty() {
                anyhow::bail!(
                    "没有可共享的目录：配置里既没有 [[servers]]，也没有 [[sftp_server.share]]。"
                );
            }

            // 主机密钥的父目录按需创建——不用 SFTP 的命令不会留下空的 keys/。
            if let Some(parent) = cfg.host_key.parent()
                && !parent.as_os_str().is_empty()
                && !parent.exists()
            {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("创建主机密钥目录 {} 失败: {e}", parent.display())
                })?;
            }
            if !cfg.host_key.exists() {
                anyhow::bail!(
                    "主机私钥不存在: {}\n\
                     生成一把：atlas keygen --path {}",
                    cfg.host_key.display(),
                    cfg.host_key.display()
                );
            }

            // 授权公钥文件不存在就自动建一个（只含说明注释），
            // 免得用户卡在「文件不存在」这一步还要自己去造文件。
            let created_authorized =
                atlas_sftp::server::ensure_authorized_keys(&cfg.authorized_keys)?;
            let authorized_fingerprints =
                atlas_sftp::server::load_authorized_keys(&cfg.authorized_keys)?;
            let authorized_count = authorized_fingerprints.len();

            println!("SFTP 服务端启动中……");
            println!("  监听:      {}:{}", cfg.listen, cfg.port);
            println!("  主机私钥:  {}", cfg.host_key.display());
            println!("  授权公钥:  {}", cfg.authorized_keys.display());
            if created_authorized {
                println!("              ↳ 已自动创建该文件（目前只有说明注释，没有公钥）");
            }
            // 主机密钥指纹：客户端首次连接会弹出"未知主机密钥"指纹，
            // 运维拿它核对才能确认连的是真服务器而不是中间人。
            match atlas_sftp::keygen::fingerprint_of_private_key(&cfg.host_key) {
                Ok(fp) => println!("  主机密钥指纹 {fp}"),
                Err(e) => tracing::warn!("计算主机密钥指纹失败: {e}"),
            }
            check_authorized_keys(&cfg.host_key, &authorized_fingerprints);
            println!("  共享目录:");
            for share in &shares {
                println!(
                    "    - {:<16} → {}{}",
                    share.name,
                    share.local_path.display(),
                    if share.read_only { "（只读）" } else { "" }
                );
            }
            println!();
            println!(
                "远端用 SFTP 客户端连接：sftp://<用户名>@<Atlas主机>:{}",
                cfg.port
            );
            if authorized_count == 0 {
                println!("（客户端只接受公钥认证）");
                println!();
                println!("⚠️  授权公钥文件里还没有任何公钥 —— 现在谁也连不上。");
                println!("    把远端电脑的客户端公钥（形如 `ssh-ed25519 AAAA... 备注`）");
                println!("    写进上面那个文件，一行一个，然后重启本服务。");
            } else {
                println!(
                    "（客户端只接受公钥认证；已授权 {} 个客户端公钥）",
                    authorized_count
                );
            }

            let options = atlas_sftp::server::SftpServerOptions {
                listen: cfg.listen.clone(),
                port: cfg.port,
                host_key: cfg.host_key.clone(),
                authorized_keys: cfg.authorized_keys.clone(),
                shares,
            };
            atlas_sftp::server::SftpServer::new(options).run().await?;
        }
        Commands::Remote => {
            let cfg = atlas.config.remote.clone();
            if !cfg.enabled {
                anyhow::bail!(
                    "配置里 [remote] 还没启用。先编辑 {} 把 enabled 改成 true。",
                    cli.config.display()
                );
            }

            // 主机私钥：远端客户端首次连接时校验的就是它。
            if !cfg.host_key.exists() {
                anyhow::bail!(
                    "主机私钥不存在: {}\n\
                     生成一把：atlas keygen --path {}",
                    cfg.host_key.display(),
                    cfg.host_key.display()
                );
            }
            let key_text = std::fs::read_to_string(&cfg.host_key)
                .with_context(|| format!("读取主机私钥 {} 失败", cfg.host_key.display()))?;
            let host_key = russh::keys::decode_secret_key(&key_text, None)
                .with_context(|| format!("解析主机私钥 {} 失败", cfg.host_key.display()))?;

            // 授权名单：与 SFTP 服务端共用同一份文件。
            // 缺失时自动建一个（只含说明注释，不含任何公钥）——
            // 与 `atlas serve` 的行为保持一致，否则用户会卡在
            // "文件不存在"这一步，连不上却不知道原因。
            let created_authorized =
                atlas_sftp::server::ensure_authorized_keys(&cfg.authorized_keys)?;
            let authorized = remote::load_authorized_keys(&cfg.authorized_keys)?;

            let (intent_tx, mut intent_rx) = tokio::sync::mpsc::channel::<remote::Intent>(64);
            let shared = Arc::new(remote::Shared {
                config: atlas.config.clone(),
                bus: atlas.bus.clone(),
                intents: intent_tx,
                states: std::sync::RwLock::new(Vec::new()),
            });

            // SSH 服务端：与本进程同体，所以远端 TUI 里的开关服作用的就是
            // 下面这份 Supervisor 托管的实例。
            let mut server = remote::RemoteServer::new(shared.clone(), authorized.clone());
            let ssh_config = Arc::new(russh::server::Config {
                keys: vec![host_key],
                // 拒绝时固定耗时，避免通过响应时间探测公钥是否存在。
                auth_rejection_time: Duration::from_secs(3),
                auth_rejection_time_initial: Some(Duration::from_secs(0)),
                ..Default::default()
            });
            let listen = cfg.listen.clone();
            let port = cfg.port;
            tokio::spawn(async move {
                if let Err(e) = server
                    .run_on_address(ssh_config, (listen.as_str(), port))
                    .await
                {
                    tracing::error!("远程 TUI 服务端异常退出: {e}");
                }
            });

            // 托管标记了 auto_start 的服务器。
            let targets = resolve_targets(&atlas.config, Vec::new(), true);
            for id in &targets {
                atlas.supervisor.start(id).await?;
            }
            let _pumps = atlas.spawn_log_pumps(false);

            println!("远程 TUI 已启动：");
            println!("  监听:      {}:{}", cfg.listen, cfg.port);
            println!("  主机私钥:  {}", cfg.host_key.display());
            println!("  授权公钥:  {}", cfg.authorized_keys.display());
            println!("  已授权:    {} 个客户端公钥", authorized.len());
            println!("  已托管:    {} 台服务器", targets.len());
            // 颜色档位：服务端看不到客户端终端的环境，只能按自己的环境判定。
            // 打印出来让运维知道客户端会看到什么，需要时用 ATLAS_COLOR_DEPTH 覆盖。
            println!(
                "  颜色档位:  {}",
                color_depth_label(atlas_tui::ColorDepth::detect())
            );
            // 主机密钥指纹：客户端首次连接会弹出"未知主机密钥"指纹，
            // 运维拿它核对才能确认连的是真服务器而不是中间人。
            match atlas_sftp::keygen::fingerprint_of_private_key(&cfg.host_key) {
                Ok(fp) => println!("  主机密钥指纹 {fp}"),
                Err(e) => tracing::warn!("计算主机密钥指纹失败: {e}"),
            }
            check_authorized_keys(&cfg.host_key, &authorized);
            println!();
            println!("远端连接（分配一个终端窗口即可看到界面）：");
            println!(
                "  ssh -tt -p {} <用户名>@<Atlas主机> {}",
                cfg.port,
                remote::TUI_COMMAND
            );
            println!("  界面里按 s 启动 / x 停止 / r 重启选中的服务器。");
            println!();
            println!("客户端必须用**私钥**登录（口令认证一律拒绝）：");
            println!(
                "  ssh -tt -i <私钥路径> -p {} <用户名>@<Atlas主机> {}",
                cfg.port,
                remote::TUI_COMMAND
            );
            println!("  私钥用 `atlas keygen` 生成；对应的公钥要写进上面那个授权公钥文件。");
            if created_authorized {
                println!("  ↳ 已自动创建授权公钥文件（目前只有说明注释，没有公钥）。");
            }
            if authorized.is_empty() {
                println!();
                println!("⚠️  授权公钥文件里没有可用公钥 —— 现在谁都连不上。");
                println!(
                    "    把远端电脑的客户端公钥写进 {}，一行一个，然后重启本服务。",
                    cfg.authorized_keys.display()
                );
            }
            println!();
            println!("（Ctrl+C 将优雅停止全部服务器）");

            // 主循环：执行会话意图 + 崩溃探测 + 等待 Ctrl+C。
            let ctrl_c = tokio::signal::ctrl_c();
            tokio::pin!(ctrl_c);
            let mut ticker = tokio::time::interval(Duration::from_secs(2));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    intent = intent_rx.recv() => {
                        let Some(intent) = intent else { break };
                        apply_intent(&mut atlas, intent).await;
                        refresh_state_snapshot(&atlas, &shared);
                    }
                    result = &mut ctrl_c => {
                        if let Err(e) = result {
                            tracing::warn!("监听 Ctrl+C 失败: {e}");
                        }
                        break;
                    }
                    _ = ticker.tick() => {
                        for (id, state) in atlas.supervisor.poll_changes() {
                            eprintln!("[{id}] 进程已退出，当前状态：{state}");
                        }
                        refresh_state_snapshot(&atlas, &shared);
                    }
                }
            }

            println!();
            println!("正在停止所有服务器…");
            let errors = atlas.supervisor.stop_all().await;
            for (id, e) in errors {
                eprintln!("停止 {id} 失败: {e}");
            }
            println!("全部服务器已停止。");
        }
    }

    Ok(())
}

/// 就绪检查里的勾 / 叉。
fn mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

/// 解析目标服务器列表：未指定时按 `auto_start` 或全部。
fn resolve_targets(config: &AtlasConfig, requested: Vec<String>, auto_only: bool) -> Vec<String> {
    if !requested.is_empty() {
        return requested;
    }
    config
        .servers
        .iter()
        .filter(|s| !auto_only || s.auto_start)
        .map(|s| s.id.clone())
        .collect()
}

/// 若配置文件存在则读取它，否则返回 `None`（**不**创建任何文件）。
fn load_config_if_exists(path: &std::path::Path) -> Result<Option<AtlasConfig>> {
    if path.exists() {
        AtlasConfig::load_from(path)
            .context("加载配置失败")
            .map(Some)
    } else {
        Ok(None)
    }
}

/// 加载配置；文件缺失时自动生成模板与必要目录。
fn load_config(path: &std::path::Path) -> Result<AtlasConfig> {
    if path.exists() {
        let config = AtlasConfig::load_from(path).context("加载配置失败")?;
        // 补建缺失目录：服务端的 workdir 缺失会导致进程启动失败，
        // 插件目录缺失会让插件发现静默返回空列表。
        for dir in scaffold::ensure_dirs(&config)? {
            println!("已创建目录: {}", dir.display());
        }
        return Ok(config);
    }

    // 首次运行：自动生成配置模板与必要目录。
    let result = scaffold::scaffold(path, &AtlasConfig::default(), false, false)?;
    if result.created_config.is_some() {
        println!("已生成配置文件: {}", path.display());
        println!("请编辑该文件添加服务器（示例已以注释形式写在文件里）。");
    }
    for dir in &result.created_dirs {
        println!("已创建目录: {}", dir.display());
    }
    Ok(AtlasConfig::default())
}

/// 没有任何服务器可启动时的提示。
///
/// 必须给出**可操作**的下一步，否则用户只会看到命令"卡住不动"。
fn print_nothing_to_start(config: &AtlasConfig) {
    if config.servers.is_empty() {
        println!("配置里还没有任何服务器，所以没有可启动的进程。");
        println!();
        println!("下一步：");
        println!(
            "  1. 编辑 atlas.toml，把 [[servers]] 段的注释去掉并填写（示例见 atlas.toml.example）"
        );
        println!("  2. 或运行 `atlas init` 生成一份带注释示例的配置");
        println!("  3. 然后运行 `atlas start <服务器ID>`");
        return;
    }
    println!(
        "没有需要启动的服务器：共配置了 {} 台，但都没有设置 auto_start = true。",
        config.servers.len()
    );
    println!();
    println!("可用的服务器：");
    for s in &config.servers {
        println!("  - {}", s.id);
    }
    println!();
    println!("用 `atlas start <服务器ID>` 显式启动其中一台。");
}

/// 启动横幅：明确告诉用户现在处于什么状态、怎么看输出、怎么退出。
fn print_start_banner(config: &AtlasConfig, targets: &[String]) {
    println!("已启动 {} 台服务器：", targets.len());
    for id in targets {
        match config.server(id) {
            Some(s) => println!("  - {id}  ({})", s.display_name()),
            None => println!("  - {id}"),
        }
    }
    println!();
    println!("以下为各服务器的实时输出（Ctrl+C 将优雅停止全部服务器）：");
}

/// 等待 Ctrl+C，然后优雅停止所有托管中的服务器。
///
/// Atlas 退出前要先把停止命令投递给子进程并等待其优雅退出，超时后再强制终止；
/// 不能让子进程被 `kill_on_drop` 直接硬杀——那会丢失服务端的世界存档。
///
/// 等待期间每 2 秒轮询一次各服务器的存活状态：某个服务端意外崩溃时立刻提示，
/// 否则运维只能看到"日志突然不动了"，很难和"卡死"区分开。
async fn wait_for_shutdown(supervisor: &mut Supervisor) {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            result = &mut ctrl_c => {
                if let Err(e) = result {
                    tracing::warn!("监听 Ctrl+C 失败: {e}");
                }
                break;
            }
            _ = ticker.tick() => {
                for (id, state) in supervisor.poll_changes() {
                    eprintln!("[{id}] 进程已退出，当前状态：{state}");
                }
            }
        }
    }

    println!();
    println!("正在停止所有服务器…");
    let errors = supervisor.stop_all().await;
    for (id, e) in errors {
        eprintln!("停止 {id} 失败: {e}");
    }
    println!("全部服务器已停止。");
}

/// 启动 TUI，并把事件流灌进界面。
async fn run_tui(atlas: &mut Atlas) -> Result<()> {
    use atlas_tui::App;

    let _pumps = atlas.spawn_log_pumps(false);

    let mut app = App::new(atlas.config.clone(), atlas.bus.clone());
    let mut bus_rx = atlas.bus.subscribe();
    let mut key_stream = crossterm::event::EventStream::new();

    let mut terminal = ratatui::init();
    let result = tui_loop(&mut terminal, &mut app, atlas, &mut bus_rx, &mut key_stream).await;
    ratatui::restore();

    // 退出 TUI 也要**优雅停止**服务器。
    //
    // 直接返回的话，`ManagedServer` 会被 drop，而子进程是 `kill_on_drop(true)`
    // ——等于硬杀 Minecraft 服务端，可能丢世界存档。`atlas start` 的 Ctrl+C
    // 走的是优雅停止路径，这里必须一致：用户按 `q` 和按 Ctrl+C 不该有
    // 不同的数据安全后果。
    let errors = atlas.supervisor.stop_all().await;
    for (id, e) in errors {
        eprintln!("停止 {id} 失败: {e}");
    }

    result
}

async fn tui_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut atlas_tui::App,
    atlas: &mut Atlas,
    bus_rx: &mut tokio::sync::broadcast::Receiver<Event>,
    key_stream: &mut crossterm::event::EventStream,
) -> Result<()> {
    use atlas_tui::AppAction;
    use crossterm::event::{Event as CrosstermEvent, KeyEventKind};
    use futures::StreamExt;

    loop {
        terminal.draw(|frame| atlas_tui::render(frame, &mut *app))?;

        tokio::select! {
            maybe = key_stream.next() => {
                let Some(Ok(CrosstermEvent::Key(key))) = maybe else { continue };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match atlas_tui::handle_key(app, key) {
                    AppAction::Quit => break,
                    AppAction::Submit => {
                        let line = app.take_input();
                        if !line.is_empty() {
                            dispatch_command(app, atlas, &line).await;
                        }
                    }
                    AppAction::StartServer(id) => {
                        // 先给反馈再干活：启动可能要拉起 JVM，
                        // 让用户知道"按了有反应"，而不是界面干等着。
                        app.status = Some(format!("正在启动 {id}…"));
                        match atlas.supervisor.start(id.as_str()).await {
                            Ok(()) => app.status = Some(format!("已启动 {id}")),
                            Err(e) => {
                                app.status = Some(format!("启动 {id} 失败：{e}{}", failure_hint(&e)))
                            }
                        }
                    }
                    AppAction::StopServer(id) => {
                        // 停止可能要等进程自行退出（最长 30 秒），
                        // 这段时间必须有明确的进度提示。
                        app.status = Some(format!("正在停止 {id}…"));
                        match atlas.supervisor.stop(id.as_str()).await {
                            Ok(()) => app.status = Some(format!("已停止 {id}")),
                            Err(e) => {
                                app.status = Some(format!("停止 {id} 失败：{e}{}", failure_hint(&e)))
                            }
                        }
                    }
                    AppAction::RestartServer(id) => {
                        app.status = Some(format!("正在重启 {id}…"));
                        match atlas.supervisor.restart(id.as_str()).await {
                            Ok(()) => app.status = Some(format!("已重启 {id}")),
                            Err(e) => {
                                app.status = Some(format!("重启 {id} 失败：{e}{}", failure_hint(&e)))
                            }
                        }
                    }
                    AppAction::ToggleHelp
                    | AppAction::ToggleView
                    | AppAction::Move(_)
                    | AppAction::ScrollLog(_)
                    | AppAction::None => {}
                }
            }
            outcome = atlas_core::EventBus::recv(bus_rx) => {
                match outcome {
                    atlas_core::RecvOutcome::Event(ev) => app.handle_event(ev),
                    atlas_core::RecvOutcome::Lagged(n) => {
                        app.status = Some(format!("事件流落后 {n} 条，请重连以重新同步"));
                    }
                    atlas_core::RecvOutcome::Closed => break,
                }
            }
        }
    }
    Ok(())
}

/// 执行一条来自远程 TUI 会话的意图。
///
/// 副作用都在这里发生——会话只负责表达意图，实际的开关服与命令执行
/// 都发生在持有 Supervisor 的主循环里。结果通过事件总线回到会话的界面。
async fn apply_intent(atlas: &mut Atlas, intent: remote::Intent) {
    use remote::Intent;

    tracing::debug!("执行意图: {intent:?}");
    match intent {
        Intent::Start(id) => match atlas.supervisor.start(&id).await {
            Ok(()) => publish_status(&atlas.bus, &id, format!("已启动 {id}")),
            Err(e) => publish_failure(&atlas.bus, &id, "启动", &e),
        },
        Intent::Stop(id) => match atlas.supervisor.stop(&id).await {
            Ok(()) => publish_status(&atlas.bus, &id, format!("已停止 {id}")),
            Err(e) => publish_failure(&atlas.bus, &id, "停止", &e),
        },
        Intent::Restart(id) => match atlas.supervisor.restart(&id).await {
            Ok(()) => publish_status(&atlas.bus, &id, format!("已重启 {id}")),
            Err(e) => publish_failure(&atlas.bus, &id, "重启", &e),
        },
        Intent::Execute { server, line } => {
            remote::execute_command(&mut atlas.supervisor, &atlas.bus, &server, &line).await;
        }
    }
}

/// 把一条状态提示发布到事件总线，让远程会话的界面能看到。
fn publish_status(bus: &EventBus, server: &str, text: String) {
    let _ = bus.publish(Event::Status {
        server: ServerId::new(server.to_owned()),
        text: text.clone(),
    });
    tracing::info!("[{server}] {text}");
}

/// 给操作失败补一句**能照着做的**建议。
///
/// 只说"失败了"没用——运维还得自己猜下一步。补上原因和建议，
/// 错误信息才真的能帮人解决问题。
fn failure_hint(error: &atlas_core::AtlasError) -> String {
    match error {
        atlas_core::AtlasError::NotManaged(_) => {
            "（该服务器不由本进程托管，请在托管它的那个 Atlas 里操作）".to_owned()
        }
        atlas_core::AtlasError::UnknownServer(_) => {
            "（配置里没有这台服务器，检查 atlas.toml 的 [[servers]]）".to_owned()
        }
        atlas_core::AtlasError::PermissionDenied { .. } => {
            "（权限不足，检查 Atlas 的授权配置）".to_owned()
        }
        _ => String::new(),
    }
}

/// 把一条失败结果连同建议发布到事件总线。
fn publish_failure(bus: &EventBus, server: &str, action: &str, error: &atlas_core::AtlasError) {
    let hint = failure_hint(error);
    publish_status(
        bus,
        server,
        format!("{action} {server} 失败：{error}{hint}"),
    );
}

/// 刷新各服务器的当前状态快照。
///
/// 事件总线上的状态变化是瞬时的，后连上来的远程会话收不到——
/// 它们靠这份快照还原「连上来时服务器已经在跑」。
fn refresh_state_snapshot(atlas: &Atlas, shared: &Arc<remote::Shared>) {
    let states = atlas.supervisor.states();
    if let Ok(mut slot) = shared.states.write() {
        *slot = states;
    }
}

/// 颜色能力等级的中文说明。
fn color_depth_label(depth: atlas_tui::ColorDepth) -> &'static str {
    match depth {
        atlas_tui::ColorDepth::None => "无颜色（NO_COLOR / TERM=dumb）",
        atlas_tui::ColorDepth::Basic => "基础 16 色",
        atlas_tui::ColorDepth::Ansi256 => "256 色",
        atlas_tui::ColorDepth::TrueColor => "真彩色",
    }
}

/// 计算主机密钥指纹，并检查授权名单里是否混入了**主机密钥自己的公钥**。
///
/// 两把钥匙是分开的：
/// - **主机密钥**（`keys/host_ed25519`）给客户端核对服务器身份；
/// - **客户端公钥**（`keys/id_ed25519.pub`）给客户端证明自己身份。
///
/// 把主机公钥放进 `authorized_keys` 是很常见的误解——它会让
/// "持有主机私钥文件的人"自动获得客户端登录资格，而且客户端用主机私钥登录
/// 在语义上就是"用服务器的身份证明自己是客户端"。发现这种情况必须明确告警，
/// 而不是让人对着一句 `Permission denied` 猜。
fn check_authorized_keys(host_key: &std::path::Path, authorized: &[String]) {
    let Ok(host_fp) = atlas_sftp::keygen::fingerprint_of_private_key(host_key) else {
        return;
    };
    if !authorized.iter().any(|f| f == &host_fp) {
        return;
    }
    println!("⚠️  授权公钥文件里包含了**主机密钥自己的公钥**。");
    println!("    主机密钥（{host_fp}）是给客户端核对服务器身份用的，");
    println!("    客户端登录要用**另一把**私钥：atlas keygen --path keys/id_ed25519");
    println!("    然后把它的公钥写进授权公钥文件。");
    println!();
}

/// 把 TUI 中输入的命令投递给对应服务器的 stdin。
async fn dispatch_command(app: &mut atlas_tui::App, atlas: &mut Atlas, line: &str) {
    use atlas_tui::AppView;

    // 在控制台视图下，命令发往当前选中的服务器。
    let target = match &app.view {
        AppView::ServerConsole(id) => id.clone(),
        AppView::Dashboard => match app.selected_server() {
            Some(row) => row.id.clone(),
            None => {
                app.status = Some("没有可用的服务器".into());
                return;
            }
        },
    };

    let Some(server) = atlas.supervisor.server_mut(target.as_str()) else {
        app.status = Some(format!("服务器 {target} 未运行"));
        return;
    };

    let bridge = ConsoleBridge::with_timeout(Duration::from_millis(1500));
    match bridge.execute(server, line).await {
        Ok(Some(output)) => {
            app.status = Some(format!("{target} → {output}"));
        }
        Ok(None) => {
            app.status = Some(format!("{target} → {line}（无返回输出）"));
        }
        Err(e) => {
            app.status = Some(format!("执行失败: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::{Event, LogLevel};

    /// 构造一个会输出一行 Minecraft 风格日志的假服务端命令。
    fn fake_server_command(line: &str) -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd".to_owned(),
                vec!["/c".to_owned(), format!("echo {line}")],
            )
        } else {
            (
                "sh".to_owned(),
                vec!["-c".to_owned(), format!("echo '{line}'")],
            )
        }
    }

    #[test]
    fn 未指定服务器时按_auto_start_筛选() {
        let mk = |id: &str, auto: bool| atlas_core::ServerConfig {
            id: id.into(),
            name: String::new(),
            workdir: std::env::temp_dir(),
            command: "cmd".into(),
            args: Vec::new(),
            env: Default::default(),
            jvm_args: Vec::new(),
            auto_start: auto,
            log_path: None,
        };
        let config = AtlasConfig {
            servers: vec![mk("a", true), mk("b", false)],
            ..Default::default()
        };

        assert_eq!(resolve_targets(&config, Vec::new(), true), vec!["a"]);
        assert_eq!(resolve_targets(&config, Vec::new(), false), vec!["a", "b"]);
        assert_eq!(
            resolve_targets(&config, vec!["b".to_owned()], true),
            vec!["b"],
            "显式指定的服务器不受 auto_start 限制"
        );
    }

    #[test]
    fn 配置校验拒绝重复的服务器_id() {
        let base = atlas_core::ServerConfig {
            id: "same".into(),
            name: String::new(),
            workdir: std::env::temp_dir(),
            command: "cmd".into(),
            args: Vec::new(),
            env: Default::default(),
            jvm_args: Vec::new(),
            auto_start: false,
            log_path: None,
        };
        let config = AtlasConfig {
            servers: vec![base.clone(), base],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    /// 端到端冒烟：子进程输出 → 切行 → 日志解析 → 事件总线。
    #[tokio::test]
    async fn 子进程日志经解析后进入事件总线() {
        let raw = "[12:34:56] [Server thread/INFO]: Done (3.214s)!";
        let (command, args) = fake_server_command(raw);

        let config = AtlasConfig {
            servers: vec![atlas_core::ServerConfig {
                id: "fake".into(),
                name: "假服务端".into(),
                workdir: std::env::temp_dir(),
                command,
                args,
                env: Default::default(),
                jvm_args: Vec::new(),
                auto_start: true,
                log_path: None,
            }],
            ..Default::default()
        };

        let mut atlas = Atlas::new(config);
        let mut rx = atlas.bus.subscribe();

        atlas.supervisor.start("fake").await.expect("启动应成功");
        let _pumps = atlas.spawn_log_pumps(false);

        // 等待事件到达（最多 10 秒）。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut received = None;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, atlas_core::EventBus::recv(&mut rx)).await {
                Ok(atlas_core::RecvOutcome::Event(Event::Log(log))) => {
                    received = Some(log);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }

        let log = received.expect("应在超时前收到一条日志事件");
        assert_eq!(log.level, LogLevel::Info);
        assert_eq!(log.message, "Done (3.214s)!");
        assert_eq!(log.thread.as_deref(), Some("Server thread"));
        assert_eq!(log.timestamp.as_deref(), Some("12:34:56"));

        let _ = atlas.supervisor.stop("fake").await;
    }

    #[test]
    fn 空配置时_start_不会挂住而是提示后返回() {
        // 回归测试：`atlas start` 在没有可启动服务器时曾经什么都不打印就永久等待，
        // 用户只看到"卡住不动"。这里锁定"目标为空 → 走提示分支 → 立即返回"的语义。
        let config = AtlasConfig::default();
        let targets = resolve_targets(&config, Vec::new(), true);
        assert!(targets.is_empty(), "空配置不应解析出任何启动目标");
        // 提示分支必须能安全执行（不 panic）。
        print_nothing_to_start(&config);
    }

    #[test]
    fn 配置有服务器但都没开自动启动时_start_不挂住() {
        let mk = |id: &str, auto: bool| atlas_core::ServerConfig {
            id: id.into(),
            name: String::new(),
            workdir: std::env::temp_dir(),
            command: "cmd".into(),
            args: Vec::new(),
            env: Default::default(),
            jvm_args: Vec::new(),
            auto_start: auto,
            log_path: None,
        };
        let config = AtlasConfig {
            servers: vec![mk("a", false)],
            ..Default::default()
        };
        assert!(resolve_targets(&config, Vec::new(), true).is_empty());
        assert_eq!(resolve_targets(&config, Vec::new(), false), vec!["a"]);
        print_nothing_to_start(&config);
    }

    #[test]
    fn 脚手架会生成配置文件与必要目录() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config_path = tmp.path().join("atlas.toml");

        let config = AtlasConfig {
            plugin_dir: tmp.path().join("plugins"),
            data_source: atlas_core::DataSource::Log {
                path: tmp.path().join("logs"),
            },
            ..Default::default()
        };

        let result =
            scaffold::scaffold(&config_path, &config, false, false).expect("脚手架不应失败");

        assert!(result.created_config.is_some(), "应生成配置文件");
        assert!(config_path.exists());
        assert!(tmp.path().join("plugins").is_dir(), "应创建插件目录");
        assert!(tmp.path().join("logs").is_dir(), "应创建日志目录");

        // 生成的配置必须能被读回，且没有服务器。
        let loaded = AtlasConfig::load_from(&config_path).expect("生成的配置应可读回");
        assert!(loaded.servers.is_empty());
    }

    #[test]
    fn 脚手架不覆盖已有内容_但会补齐缺失段落() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config_path = tmp.path().join("atlas.toml");
        std::fs::write(&config_path, "# 我自己写的配置\n").expect("应能写入");

        let result = scaffold::scaffold(&config_path, &AtlasConfig::default(), false, false)
            .expect("脚手架不应失败");

        let content = std::fs::read_to_string(&config_path).expect("应能读取");
        assert!(
            content.starts_with("# 我自己写的配置\n"),
            "用户原有内容必须保留，实际：{content}"
        );
        assert!(
            content.contains("[sftp_server]"),
            "缺失的段落应被补齐，实际：{content}"
        );
        assert!(result.created_config.is_some(), "补齐后应报告配置被改动");
    }

    #[test]
    fn 脚手架对完整的已有配置是幂等的() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config_path = tmp.path().join("atlas.toml");
        // 一份已经带 [sftp_server] 的配置——不该被再动一次。
        let original = "# 我自己写的配置\n\n[sftp_server]\nenabled = false\n";
        std::fs::write(&config_path, original).expect("应能写入");

        let result = scaffold::scaffold(&config_path, &AtlasConfig::default(), false, false)
            .expect("脚手架不应失败");

        assert!(result.created_config.is_none(), "完整配置不应被改动");
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("应能读取"),
            original
        );
    }

    #[test]
    fn 脚手架_force_会覆盖已有配置() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config_path = tmp.path().join("atlas.toml");
        std::fs::write(&config_path, "# 旧配置\n").expect("应能写入");

        let result = scaffold::scaffold(&config_path, &AtlasConfig::default(), true, false)
            .expect("脚手架不应失败");

        assert!(result.created_config.is_some());
        let content = std::fs::read_to_string(&config_path).expect("应能读取");
        assert!(content.contains("[data_source]"));
    }

    /// 构造一份把授权公钥指向临时目录的配置，避免污染工作区。
    fn 授权公钥指向临时目录(tmp: &std::path::Path) -> AtlasConfig {
        let mut config = AtlasConfig::default();
        config.sftp_server.authorized_keys = tmp.join("keys").join("authorized_keys");
        config
    }

    #[test]
    fn 授权公钥文件不存在时脚手架会自动创建() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config = 授权公钥指向临时目录(tmp.path());

        let created = scaffold::ensure_authorized_keys_file(&config).expect("创建不应失败");

        assert!(created.is_some(), "文件此前不存在，应报告已创建");
        let path = &config.sftp_server.authorized_keys;
        assert!(path.is_file(), "授权公钥文件应被建出来：{}", path.display());
    }

    #[test]
    fn 授权公钥文件已存在时脚手架保持幂等且不覆盖() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config = 授权公钥指向临时目录(tmp.path());
        let path = config.sftp_server.authorized_keys.clone();
        std::fs::create_dir_all(path.parent().expect("应有父目录")).expect("应能创建目录");
        let original = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample 远端电脑\n";
        std::fs::write(&path, original).expect("应能写入");

        let created = scaffold::ensure_authorized_keys_file(&config).expect("创建不应失败");

        assert!(created.is_none(), "文件已存在，不应报告创建");
        assert_eq!(
            std::fs::read_to_string(&path).expect("应能读取"),
            original,
            "已有内容必须原样保留"
        );
    }
}
