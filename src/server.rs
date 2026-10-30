//! MCP server：工具定義（PLAN §4）。

use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::{Audit, AuditRecord, now_rfc3339};
use crate::config::{Config, ShellMode};
use crate::guard::{Guard, Verdict};
use crate::output;
use crate::ssh::{exec, pool::Pool, state::CwdState, wrap};

pub struct AppCtx {
    pub config: Config,
    pub pool: Arc<Pool>,
    pub guard: Guard,
    pub audit: Audit,
    pub cwd: CwdState,
}

#[derive(Clone)]
pub struct SshMcp {
    ctx: Arc<AppCtx>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExecArgs {
    #[schemars(description = "主機別名（用 ssh_list_hosts 查詢可用的 alias）")]
    pub host: String,
    #[schemars(
        description = "要執行的 shell 命令，可多行。cd 會被記住到下一次呼叫；環境變數不會保留"
    )]
    pub command: String,
    #[schemars(description = "逾時秒數（預設由設定檔決定，超過上限會被限制）")]
    pub timeout_s: Option<u64>,
    #[schemars(
        description = "這次改在指定目錄執行（可為相對於目前目錄的路徑）；成功後成為新的記住目錄"
    )]
    pub cwd: Option<String>,
    #[schemars(description = "true = 執行前先把記住的目錄重設為設定檔的初始目錄")]
    pub reset_cwd: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ExecResult {
    exit_code: Option<u32>,
    exit_signal: Option<String>,
    stdout: String,
    stderr: String,
    cwd: Option<String>,
    cwd_updated: bool,
    truncated: bool,
    full_output_path: Option<String>,
    duration_ms: u128,
    timed_out: bool,
}

fn error_result(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

#[tool_router]
impl SshMcp {
    pub fn new(ctx: Arc<AppCtx>) -> Self {
        Self {
            ctx,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "列出設定檔中可連線的 SSH 主機（alias、host、port、帳號、目前記住的工作目錄等）",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn ssh_list_hosts(&self) -> CallToolResult {
        let c = &self.ctx;
        let hosts: Vec<_> = c
            .config
            .hosts
            .iter()
            .map(|h| {
                let cwd = match h.shell_mode {
                    ShellMode::Raw => None,
                    ShellMode::Posix => c.cwd.get(&h.alias, h.cwd.as_deref()),
                };
                json!({
                    "alias": h.alias,
                    "description": h.description,
                    "host": h.host,
                    "port": h.port,
                    "user": h.user,
                    "cwd": cwd,
                    "has_pre_command": h.pre_command.is_some(),
                    "env_keys": h.env.keys().collect::<Vec<_>>(),
                    "shell_mode": h.shell_mode,
                    "allow_dangerous": h.allow_dangerous,
                })
            })
            .collect();
        c.audit.write(&AuditRecord {
            ts: now_rfc3339(),
            tool: "ssh_list_hosts",
            ..Default::default()
        });
        CallToolResult::structured(json!({ "hosts": hosts }))
    }

    #[tool(
        description = "在指定主機上透過 SSH 執行 shell 命令，回傳 exit_code、stdout、stderr。\
每次都是新的 shell：cd 的結果會被 server 記住並套用到下一次呼叫，但 export 的環境變數不會保留\
（需要的環境請在同一個命令中設定，或由使用者在設定檔加入 env / pre_command）。\
不支援互動式命令（vim、top、需要輸入密碼的 sudo 等）。",
        annotations(
            destructive_hint = true,
            open_world_hint = true,
            read_only_hint = false
        )
    )]
    async fn ssh_exec(&self, Parameters(args): Parameters<ExecArgs>) -> CallToolResult {
        let c = &self.ctx;
        let g = &c.config.general;

        // ---- 主機白名單 ----
        let Some(profile) = c.config.host(&args.host) else {
            let msg = format!(
                "未知的主機別名「{}」。可用的 alias：{}",
                args.host,
                c.config.aliases().join(", ")
            );
            c.audit.write(&AuditRecord {
                ts: now_rfc3339(),
                tool: "ssh_exec",
                host: Some(&args.host),
                command: Some(&args.command),
                error: Some(&msg),
                ..Default::default()
            });
            return error_result(msg);
        };

        if args.command.trim().is_empty() {
            return error_result("command 不可為空");
        }

        // ---- guard ----
        let mut dangerous_rule: Option<String> = None;
        match c.guard.check(&args.command, profile.allow_dangerous) {
            Verdict::Allowed => {}
            Verdict::AllowedDangerous(rule) => {
                tracing::warn!(alias = %profile.alias, %rule, "危險命令已依設定放行");
                dangerous_rule = Some(rule);
            }
            Verdict::Blocked(rule) => {
                c.audit.write(&AuditRecord {
                    ts: now_rfc3339(),
                    tool: "ssh_exec",
                    host: Some(&profile.alias),
                    command: Some(&args.command),
                    blocked: true,
                    blocked_rule: Some(&rule),
                    ..Default::default()
                });
                return error_result(format!(
                    "命令被安全規則擋下（規則：{rule}）。這類操作請使用者自行在終端機執行。"
                ));
            }
        }

        // ---- cwd ----
        let posix = profile.shell_mode == ShellMode::Posix;
        if args.reset_cwd.unwrap_or(false) {
            c.cwd.reset(&profile.alias);
        }
        let remembered = posix
            .then(|| c.cwd.get(&profile.alias, profile.cwd.as_deref()))
            .flatten();
        let override_cwd = args
            .cwd
            .as_deref()
            .filter(|s| !s.trim().is_empty() && posix);

        let nonce = wrap::new_nonce();
        let script = wrap::build_command(
            profile,
            remembered.as_deref(),
            override_cwd,
            &args.command,
            &nonce,
        );
        let timeout_s = args
            .timeout_s
            .unwrap_or(g.default_timeout_s)
            .clamp(1, g.max_timeout_s);

        // ---- 執行（連線失效時重連一次）----
        let mut attempt = 0;
        let raw = loop {
            attempt += 1;
            let session = match c.pool.get(profile).await {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("{e:#}");
                    c.audit.write(&AuditRecord {
                        ts: now_rfc3339(),
                        tool: "ssh_exec",
                        host: Some(&profile.alias),
                        command: Some(&args.command),
                        error: Some(&msg),
                        ..Default::default()
                    });
                    return error_result(msg);
                }
            };
            let permit = session.channels.acquire().await;
            let r = exec::run(&session.handle, &script, Duration::from_secs(timeout_s)).await;
            drop(permit);
            match r {
                Ok(raw) => break raw,
                Err(e) if attempt == 1 => {
                    tracing::warn!(alias = %profile.alias, "執行失敗，重新連線後重試：{e:#}");
                    c.pool.invalidate(&profile.alias).await;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    c.audit.write(&AuditRecord {
                        ts: now_rfc3339(),
                        tool: "ssh_exec",
                        host: Some(&profile.alias),
                        command: Some(&args.command),
                        error: Some(&msg),
                        ..Default::default()
                    });
                    return error_result(msg);
                }
            }
        };

        // ---- 解析（先解析完整輸出，再截斷）----
        let stdout_full = String::from_utf8_lossy(&raw.stdout).into_owned();
        let stderr_full = String::from_utf8_lossy(&raw.stderr).into_owned();
        let (stdout_full, stderr_full, new_cwd, wrap_error) = if posix {
            let p = wrap::parse_output(&stdout_full, &stderr_full, raw.exit_code, &nonce);
            (p.stdout, p.stderr, p.new_cwd, p.wrap_error)
        } else {
            (stdout_full, stderr_full, None, None)
        };

        let cwd_updated = new_cwd.is_some();
        if let Some(cwd) = &new_cwd {
            c.cwd.set(&profile.alias, cwd.clone());
        }
        let cwd_now = posix
            .then(|| c.cwd.get(&profile.alias, profile.cwd.as_deref()))
            .flatten();

        let (stdout, t1) = output::truncate(&stdout_full, g.max_output_bytes);
        let (stderr, t2) = output::truncate(&stderr_full, g.max_output_bytes);
        let truncated = t1 || t2;
        let full_output_path = if truncated {
            match output::save_full(
                &g.output_dir,
                &profile.alias,
                &args.command,
                &stdout_full,
                &stderr_full,
            ) {
                Ok(p) => Some(p.display().to_string()),
                Err(e) => {
                    tracing::error!("儲存完整輸出失敗：{e}");
                    None
                }
            }
        } else {
            None
        };

        let wrap_msg = wrap_error.map(|w| match w {
            wrap::WrapError::Cwd => format!(
                "無法進入工作目錄「{}」（可能已被刪除或沒有權限）。請用 reset_cwd: true 重設，或用 cwd 參數指定其他目錄。",
                override_cwd.or(remembered.as_deref()).unwrap_or("?")
            ),
            wrap::WrapError::PreCommand => format!(
                "主機「{}」設定檔中的 pre_command 執行失敗，命令沒有被執行。請使用者檢查設定檔（{}）。",
                profile.alias,
                c.config.path.display()
            ),
        });

        c.audit.write(&AuditRecord {
            ts: now_rfc3339(),
            tool: "ssh_exec",
            host: Some(&profile.alias),
            command: Some(&args.command),
            cwd: cwd_now.as_deref(),
            exit_code: raw.exit_code,
            exit_signal: raw.exit_signal.as_deref(),
            duration_ms: Some(raw.duration_ms),
            dangerous_allowed: dangerous_rule.is_some(),
            blocked_rule: dangerous_rule.as_deref(),
            timed_out: raw.timed_out,
            truncated,
            error: wrap_msg.as_deref(),
            ..Default::default()
        });

        let result = ExecResult {
            exit_code: raw.exit_code,
            exit_signal: raw.exit_signal,
            stdout,
            stderr,
            cwd: cwd_now,
            cwd_updated,
            truncated,
            full_output_path,
            duration_ms: raw.duration_ms,
            timed_out: raw.timed_out,
        };
        let mut value = serde_json::to_value(&result).unwrap_or_else(|_| json!({}));
        match wrap_msg {
            Some(msg) => {
                value["error"] = json!(msg);
                CallToolResult::structured_error(value)
            }
            None => CallToolResult::structured(value),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SshMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            // from_build_env() 會取到 rmcp 自己的名稱與版本，所以明確指定
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "透過 SSH 在使用者設定的主機上執行命令。先用 ssh_list_hosts 查看可用主機，\
再用 ssh_exec 執行。每次呼叫都是新的 shell：工作目錄會被記住，環境變數不會。\
請把相關步驟用 && 串在同一個命令中；不要執行互動式命令。",
            )
    }
}
