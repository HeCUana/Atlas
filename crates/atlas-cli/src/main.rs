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
use std::time::Duration;

use anyhow::{Context, Result};
use atlas_core::{AtlasConfig, Event, EventBus, LogLevel, ServerId};
use atlas_parser::LogParser;
use atlas_server::{ConsoleBridge, Supervisor};
use clap::{Parser, Subcommand};

mod scaffold;

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
    fn spawn_log_pumps(&self) -> Vec<tokio::task::JoinHandle<()>> {
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
            for id in &targets {
                atlas.supervisor.start(id).await?;
                println!("已启动 {id}");
            }
            let _pumps = atlas.spawn_log_pumps();
            wait_forever().await;
        }
        Commands::Stop { servers } => {
            let targets = resolve_targets(&atlas.config, servers, false);
            for id in &targets {
                atlas.supervisor.stop(id).await?;
                println!("已停止 {id}");
            }
        }
        Commands::Restart { servers } => {
            let targets = resolve_targets(&atlas.config, servers, false);
            for id in &targets {
                atlas.supervisor.restart(id).await?;
                println!("已重启 {id}");
            }
            let _pumps = atlas.spawn_log_pumps();
            wait_forever().await;
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
    }

    Ok(())
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

async fn wait_forever() {
    let _ = tokio::signal::ctrl_c().await;
}

/// 启动 TUI，并把事件流灌进界面。
async fn run_tui(atlas: &mut Atlas) -> Result<()> {
    use atlas_tui::App;

    let _pumps = atlas.spawn_log_pumps();

    let mut app = App::new(atlas.config.clone(), atlas.bus.clone());
    let mut bus_rx = atlas.bus.subscribe();
    let mut key_stream = crossterm::event::EventStream::new();

    let mut terminal = ratatui::init();
    let result = tui_loop(&mut terminal, &mut app, atlas, &mut bus_rx, &mut key_stream).await;
    ratatui::restore();
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
        terminal.draw(|frame| atlas_tui::render(frame, app))?;

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
                    AppAction::ToggleView | AppAction::Move(_) | AppAction::None => {}
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
        let _pumps = atlas.spawn_log_pumps();

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
    fn 脚手架是幂等的_不会覆盖已有配置() {
        let tmp = tempfile::tempdir().expect("应能创建临时目录");
        let config_path = tmp.path().join("atlas.toml");
        std::fs::write(&config_path, "# 我自己写的配置\n").expect("应能写入");

        let result = scaffold::scaffold(&config_path, &AtlasConfig::default(), false, false)
            .expect("脚手架不应失败");

        assert!(result.created_config.is_none(), "已有配置不应被覆盖");
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("应能读取"),
            "# 我自己写的配置\n"
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
}
