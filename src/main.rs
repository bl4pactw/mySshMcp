//! ssh-mcp：個人用 SSH MCP server（STDIO transport）。
//!
//! 重要：MCP 模式下 stdout 只能輸出 JSON-RPC。所有 log 一律寫到 stderr 與檔案；
//! 程式中禁止使用 `println!`（由 clippy::print_stdout lint 把關，只有 CLI 子命令例外）。

#![deny(clippy::print_stdout)]

mod audit;
mod config;
mod guard;
mod output;
mod server;
mod ssh;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::server::{AppCtx, SshMcp};

const USAGE: &str = "\
ssh-mcp — 個人用 SSH MCP server（STDIO）

用法：
  ssh-mcp [--config <path>]                      啟動 MCP server（由 MCP client 呼叫）
  ssh-mcp --check-config [--connect] [--config <path>]
                                                 驗證設定檔、ssh-agent；--connect 會實際連線每台主機
  ssh-mcp --version
  ssh-mcp --help

設定檔位置（優先序）：--config → 環境變數 SSH_MCP_CONFIG → %APPDATA%\\ssh-mcp\\config.toml
";

struct Args {
    config: Option<PathBuf>,
    check: bool,
    connect: bool,
}

#[allow(clippy::print_stdout)]
fn parse_args() -> Result<Option<Args>> {
    let mut args = Args {
        config: None,
        check: false,
        connect: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" | "-V" => {
                println!("ssh-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--check-config" => args.check = true,
            "--connect" => args.connect = true,
            "--config" => {
                args.config = Some(PathBuf::from(it.next().context("--config 需要指定路徑")?));
            }
            other => anyhow::bail!("未知的參數：{other}\n\n{USAGE}"),
        }
    }
    Ok(Some(args))
}

/// log 寫到 stderr + `<log_dir>/ssh-mcp.log.YYYY-MM-DD`
fn init_logging(log_dir: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_env("SSH_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file = std::fs::create_dir_all(log_dir)
        .ok()
        .map(|_| tracing_appender::rolling::daily(log_dir, "ssh-mcp.log"));
    match file {
        Some(appender) => {
            let (nb, guard) = tracing_appender::non_blocking(appender);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::io::stderr.and(nb))
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::io::stderr)
                .init();
            None
        }
    }
}

#[tokio::main]
async fn main() {
    // panic 訊息導向 stderr（預設即為 stderr，這裡額外寫進 log）
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
        eprintln!("ssh-mcp panic: {info}");
    }));

    if let Err(e) = run().await {
        eprintln!("ssh-mcp 錯誤：{e:#}");
        tracing::error!("{e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let path = config::resolve_config_path(args.config.as_deref());
    let cfg = config::load(&path)?;

    if args.check {
        return check_config(cfg, args.connect).await;
    }

    let _log_guard = init_logging(&cfg.general.log_dir);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %cfg.path.display(),
        hosts = cfg.hosts.len(),
        "ssh-mcp 啟動"
    );

    let pool = ssh::pool::Pool::new(cfg.general.clone());
    pool.spawn_reaper();
    let ctx = Arc::new(AppCtx {
        guard: guard::Guard::new(&cfg.deny_patterns)?,
        audit: audit::Audit::new(cfg.general.log_dir.clone()),
        cwd: Default::default(),
        pool,
        config: cfg,
    });

    let service = SshMcp::new(ctx)
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP 初始化失敗")?;
    service.waiting().await?;
    tracing::info!("ssh-mcp 結束");
    Ok(())
}

/// `--check-config`：列出套用預設值後的最終設定、檢查 agent；`--connect` 會實際連線
#[allow(clippy::print_stdout)]
async fn check_config(cfg: config::Config, connect: bool) -> Result<()> {
    println!("設定檔：{}", cfg.path.display());
    println!("known_hosts：{}", cfg.general.known_hosts.display());
    println!("log 目錄：{}", cfg.general.log_dir.display());
    println!("agent：{:?}", cfg.general.agent);
    println!();

    for h in &cfg.hosts {
        println!("[{}] {}", h.alias, h.description);
        println!("  連線：{}@{}:{}", h.user, h.host, h.port);
        println!("  shell_mode：{:?}", h.shell_mode);
        println!("  cwd：{}", h.cwd.as_deref().unwrap_or("（遠端登入目錄）"));
        if let Some(p) = &h.pre_command {
            println!("  pre_command：{}", p.replace('\n', "\n               "));
        }
        if !h.env.is_empty() {
            let keys: Vec<_> = h.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!("  env：{}", keys.join(" "));
        }
        println!("  allow_dangerous：{}", h.allow_dangerous);
    }
    println!();

    let mut ok = true;
    match ssh::client::Agent::connect(&cfg.general).await {
        Ok(mut a) => match a.identities().await {
            Ok(ids) if ids.is_empty() => {
                ok = false;
                println!("✗ ssh-agent 已連線，但沒有金鑰（請執行 ssh-add）");
            }
            Ok(ids) => {
                println!("✓ ssh-agent：{} 把金鑰", ids.len());
                for id in ids {
                    let k = id.public_key();
                    println!(
                        "    {} {} {}",
                        k.algorithm().as_str(),
                        k.fingerprint(russh::keys::HashAlg::Sha256),
                        id.comment()
                    );
                }
            }
            Err(e) => {
                ok = false;
                println!("✗ {e:#}");
            }
        },
        Err(e) => {
            ok = false;
            println!("✗ {e:#}");
        }
    }

    if connect {
        println!();
        for h in &cfg.hosts {
            print!("連線測試 [{}] ... ", h.alias);
            use std::io::Write;
            let _ = std::io::stdout().flush();
            match ssh::client::connect(&cfg.general, h).await {
                Ok(handle) => {
                    match ssh::exec::run(&handle, "echo ok", Duration::from_secs(10)).await {
                        Ok(r) if r.exit_code == Some(0) => println!("✓"),
                        Ok(r) => {
                            ok = false;
                            println!(
                                "✗ exit_code={:?} stderr={}",
                                r.exit_code,
                                String::from_utf8_lossy(&r.stderr)
                            );
                        }
                        Err(e) => {
                            ok = false;
                            println!("✗ {e:#}");
                        }
                    }
                }
                Err(e) => {
                    ok = false;
                    println!("✗ {e:#}");
                }
            }
        }
    }

    println!();
    if ok {
        println!("檢查完成：沒有發現問題");
        Ok(())
    } else {
        anyhow::bail!("檢查發現問題，請依上方訊息修正")
    }
}
