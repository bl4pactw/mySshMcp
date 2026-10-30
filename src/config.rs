//! 設定檔載入、預設值合併與驗證（PLAN §5）。
//!
//! 設定檔位置（優先序）：
//!   1. 命令列 `--config <path>`
//!   2. 環境變數 `SSH_MCP_CONFIG`
//!   3. `<config_dir>/ssh-mcp/config.toml`（Windows：`%APPDATA%\ssh-mcp\config.toml`）

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// 內建預設值
const DEFAULT_PORT: u16 = 22;
const DEFAULT_CONNECT_TIMEOUT_S: u64 = 10;
const DEFAULT_KEEPALIVE_S: u64 = 30;

// ---------------------------------------------------------------------------
// 設定檔原始結構（直接對應 TOML）
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    general: RawGeneral,
    #[serde(default)]
    defaults: RawHostCommon,
    #[serde(default)]
    hosts: Vec<RawHost>,
    #[serde(default)]
    guard: RawGuard,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGeneral {
    default_timeout_s: Option<u64>,
    max_timeout_s: Option<u64>,
    max_output_bytes: Option<usize>,
    known_hosts: Option<String>,
    agent: Option<String>,
    agent_pipe: Option<String>,
    log_dir: Option<String>,
    output_dir: Option<String>,
    idle_session_ttl_s: Option<u64>,
    max_channels_per_host: Option<usize>,
}

/// `[defaults]` 與 `[[hosts]]` 共用的欄位
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHostCommon {
    port: Option<u16>,
    user: Option<String>,
    connect_timeout_s: Option<u64>,
    keepalive_s: Option<u64>,
    cwd: Option<String>,
    pre_command: Option<String>,
    shell_mode: Option<ShellMode>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHost {
    alias: String,
    #[serde(default)]
    description: String,
    host: String,
    #[serde(default)]
    allow_dangerous: bool,
    port: Option<u16>,
    user: Option<String>,
    connect_timeout_s: Option<u64>,
    keepalive_s: Option<u64>,
    cwd: Option<String>,
    pre_command: Option<String>,
    shell_mode: Option<ShellMode>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGuard {
    deny_patterns: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// 解析後的設定
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellMode {
    /// 使用包裝 script（支援 cwd 記憶、env、pre_command）
    #[default]
    Posix,
    /// 原樣送出命令
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Windows OpenSSH Agent（named pipe）
    OpenSsh,
    /// PuTTY Pageant（僅 Windows）
    Pageant,
    /// 使用 SSH_AUTH_SOCK（Unix，主要供測試）
    Env,
}

#[derive(Debug, Clone)]
pub struct General {
    pub default_timeout_s: u64,
    pub max_timeout_s: u64,
    pub max_output_bytes: usize,
    pub known_hosts: PathBuf,
    pub agent: AgentKind,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub agent_pipe: String,
    pub log_dir: PathBuf,
    pub output_dir: PathBuf,
    pub idle_session_ttl_s: u64,
    pub max_channels_per_host: usize,
}

/// 套用 `[defaults]` 後的單一主機設定
#[derive(Debug, Clone)]
pub struct HostProfile {
    pub alias: String,
    pub description: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub connect_timeout_s: u64,
    pub keepalive_s: u64,
    pub cwd: Option<String>,
    pub pre_command: Option<String>,
    pub env: BTreeMap<String, String>,
    pub shell_mode: ShellMode,
    pub allow_dangerous: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub path: PathBuf,
    pub general: General,
    pub hosts: Vec<HostProfile>,
    pub deny_patterns: Vec<String>,
}

impl Config {
    pub fn host(&self, alias: &str) -> Option<&HostProfile> {
        self.hosts.iter().find(|h| h.alias == alias)
    }

    pub fn aliases(&self) -> Vec<&str> {
        self.hosts.iter().map(|h| h.alias.as_str()).collect()
    }
}

/// 未設定 `[guard].deny_patterns` 時使用的預設黑名單（PLAN §5）
pub fn default_deny_patterns() -> Vec<String> {
    [
        r"(^|[;&|\s])rm\s+-[a-zA-Z]*r[a-zA-Z]*f?\s+/(\s|$|\*)",
        r"\bmkfs(\.\w+)?\b",
        r"\bdd\b.*\bof=/dev/",
        r"\b(shutdown|poweroff|halt|reboot)\b",
        r":\(\)\s*\{\s*:\|:&\s*\};:",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 程式資料目錄：Windows 為 `%APPDATA%\ssh-mcp`，Linux 為 `~/.config/ssh-mcp`
pub fn app_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join("ssh-mcp"))
        .unwrap_or_else(|| PathBuf::from("ssh-mcp"))
}

/// 空字串或未替換的 `${...}` 佔位字串視為「未設定」
pub fn is_unset_value(v: &str) -> bool {
    let v = v.trim();
    v.is_empty() || (v.starts_with("${") && v.ends_with('}'))
}

/// 決定設定檔路徑
pub fn resolve_config_path(cli: Option<&Path>) -> PathBuf {
    if let Some(p) = cli {
        return p.to_path_buf();
    }
    // 未填寫的 .mcpb user_config 會以原樣「${user_config.xxx}」傳進來，視同未設定
    if let Ok(p) = std::env::var("SSH_MCP_CONFIG")
        && !is_unset_value(&p)
    {
        return PathBuf::from(p.trim());
    }
    app_dir().join("config.toml")
}

/// 展開 `~` 開頭與 `%VAR%` / `${VAR}` 形式的環境變數
pub fn expand_path(input: &str) -> PathBuf {
    let mut s = input.to_string();

    // %VAR%（Windows 慣用）
    let re_pct = regex::Regex::new(r"%([A-Za-z_][A-Za-z0-9_]*)%").expect("valid regex");
    s = re_pct
        .replace_all(&s, |c: &regex::Captures| {
            std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())
        })
        .into_owned();
    // ${VAR}
    let re_brace = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("valid regex");
    s = re_brace
        .replace_all(&s, |c: &regex::Captures| {
            std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())
        })
        .into_owned();

    if (s == "~" || s.starts_with("~/") || s.starts_with("~\\"))
        && let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
    {
        let rest = s[1..].trim_start_matches(['/', '\\']);
        return if rest.is_empty() {
            home
        } else {
            home.join(rest)
        };
    }
    PathBuf::from(s)
}

fn valid_alias(a: &str) -> bool {
    !a.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_env_key(k: &str) -> bool {
    let mut chars = k.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 讀取並驗證設定檔
pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("無法讀取設定檔 {}", path.display()))?;
    parse(&text, path)
}

/// 解析設定檔內容（與檔案系統分離，方便單元測試）
pub fn parse(text: &str, path: &Path) -> Result<Config> {
    let raw: RawConfig =
        toml::from_str(text).with_context(|| format!("設定檔格式錯誤：{}", path.display()))?;

    // ---- general ----
    let g = raw.general;
    let default_timeout_s = g.default_timeout_s.unwrap_or(30);
    let max_timeout_s = g.max_timeout_s.unwrap_or(600);
    if default_timeout_s == 0 || max_timeout_s == 0 || default_timeout_s > max_timeout_s {
        bail!("[general] default_timeout_s / max_timeout_s 必須 > 0，且 default ≤ max");
    }
    let agent = match g
        .agent
        .as_deref()
        .unwrap_or("auto")
        .to_ascii_lowercase()
        .as_str()
    {
        "openssh" => AgentKind::OpenSsh,
        "pageant" => AgentKind::Pageant,
        "env" => AgentKind::Env,
        "auto" => {
            if cfg!(windows) {
                AgentKind::OpenSsh
            } else {
                AgentKind::Env
            }
        }
        other => {
            bail!("[general] agent 不支援的值：{other}（可用 auto / openssh / pageant / env）")
        }
    };
    let app = app_dir();
    let general = General {
        default_timeout_s,
        max_timeout_s,
        max_output_bytes: g.max_output_bytes.unwrap_or(32 * 1024).max(1024),
        known_hosts: expand_path(g.known_hosts.as_deref().unwrap_or("~/.ssh/known_hosts")),
        agent,
        agent_pipe: g
            .agent_pipe
            .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".to_string()),
        log_dir: g
            .log_dir
            .as_deref()
            .map(expand_path)
            .unwrap_or_else(|| app.join("logs")),
        output_dir: g
            .output_dir
            .as_deref()
            .map(expand_path)
            .unwrap_or_else(|| app.join("outputs")),
        idle_session_ttl_s: g.idle_session_ttl_s.unwrap_or(300),
        max_channels_per_host: g.max_channels_per_host.unwrap_or(4).clamp(1, 10),
    };

    // ---- hosts ----
    let d = raw.defaults;
    let mut seen = HashSet::new();
    let mut hosts = Vec::new();
    for h in raw.hosts {
        if !valid_alias(&h.alias) {
            bail!("alias「{}」不合法：只允許英數字與 . _ -", h.alias);
        }
        if !seen.insert(h.alias.clone()) {
            bail!("alias「{}」重複", h.alias);
        }
        if h.host.trim().is_empty() {
            bail!("alias「{}」缺少 host", h.alias);
        }
        let c = &h;
        let user = c
            .user
            .clone()
            .or_else(|| d.user.clone())
            .ok_or_else(|| anyhow!("alias「{}」沒有設定 user，[defaults] 也沒有", h.alias))?;
        let port = c.port.or(d.port).unwrap_or(DEFAULT_PORT);
        if port == 0 {
            bail!("alias「{}」port 不可為 0", h.alias);
        }
        let cwd = c.cwd.clone().or_else(|| d.cwd.clone());
        if let Some(cwd) = &cwd
            && !cwd.starts_with('/')
        {
            bail!("alias「{}」cwd 必須是絕對路徑（/ 開頭）：{cwd}", h.alias);
        }
        // env：defaults 與 host 合併，同名以 host 為準
        let mut env = d.env.clone();
        env.extend(c.env.clone());
        for k in env.keys() {
            if !valid_env_key(k) {
                bail!("alias「{}」env 名稱不合法：{k}", h.alias);
            }
        }
        let connect_timeout_s = c
            .connect_timeout_s
            .or(d.connect_timeout_s)
            .unwrap_or(DEFAULT_CONNECT_TIMEOUT_S)
            .max(1);
        let keepalive_s = c
            .keepalive_s
            .or(d.keepalive_s)
            .unwrap_or(DEFAULT_KEEPALIVE_S);
        hosts.push(HostProfile {
            alias: h.alias.clone(),
            description: h.description.clone(),
            host: h.host.trim().to_string(),
            port,
            user,
            connect_timeout_s,
            keepalive_s,
            cwd,
            pre_command: c
                .pre_command
                .clone()
                .or_else(|| d.pre_command.clone())
                .filter(|s| !s.trim().is_empty()),
            env,
            shell_mode: c.shell_mode.or(d.shell_mode).unwrap_or_default(),
            allow_dangerous: h.allow_dangerous,
        });
    }

    // ---- guard ----
    let deny_patterns = raw
        .guard
        .deny_patterns
        .unwrap_or_else(default_deny_patterns);
    for p in &deny_patterns {
        regex::Regex::new(p)
            .with_context(|| format!("[guard] deny_patterns 正規表示式錯誤：{p}"))?;
    }

    Ok(Config {
        path: path.to_path_buf(),
        general,
        hosts,
        deny_patterns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[general]
default_timeout_s = 20

[defaults]
port = 22
user = "builder"
cwd = "/home/builder"
[defaults.env]
LANG = "C.UTF-8"
A = "1"

[[hosts]]
alias = "build-01"
host = "build-01.lan"

[[hosts]]
alias = "lab-rpi"
host = "192.168.10.50"
port = 2222
user = "pi"
cwd = "/tmp"
shell_mode = "raw"
[hosts.env]
A = "2"
B = "x"
"#;

    #[test]
    fn defaults_are_merged() {
        let c = parse(SAMPLE, Path::new("t.toml")).unwrap();
        let b = c.host("build-01").unwrap();
        assert_eq!((b.port, b.user.as_str()), (22, "builder"));
        assert_eq!(b.cwd.as_deref(), Some("/home/builder"));
        assert_eq!(b.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(b.shell_mode, ShellMode::Posix);

        let r = c.host("lab-rpi").unwrap();
        assert_eq!((r.port, r.user.as_str()), (2222, "pi"));
        assert_eq!(r.cwd.as_deref(), Some("/tmp"));
        assert_eq!(r.env.get("A").map(String::as_str), Some("2"));
        assert_eq!(r.env.get("B").map(String::as_str), Some("x"));
        assert_eq!(r.env.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(r.shell_mode, ShellMode::Raw);
        assert_eq!(c.general.default_timeout_s, 20);
    }

    #[test]
    fn rejects_bad_input() {
        let dup =
            "[[hosts]]\nalias='a'\nhost='h'\nuser='u'\n[[hosts]]\nalias='a'\nhost='h'\nuser='u'\n";
        assert!(parse(dup, Path::new("t")).is_err());
        let rel = "[[hosts]]\nalias='a'\nhost='h'\nuser='u'\ncwd='rel/path'\n";
        assert!(parse(rel, Path::new("t")).is_err());
        let nouser = "[[hosts]]\nalias='a'\nhost='h'\n";
        assert!(parse(nouser, Path::new("t")).is_err());
        let badenv = "[[hosts]]\nalias='a'\nhost='h'\nuser='u'\n[hosts.env]\n'1X'='v'\n";
        assert!(parse(badenv, Path::new("t")).is_err());
        let unknown = "[general]\nfoo=1\n";
        assert!(parse(unknown, Path::new("t")).is_err());
    }

    #[test]
    fn unset_values() {
        assert!(is_unset_value(""));
        assert!(is_unset_value("  "));
        assert!(is_unset_value("${user_config.config_path}"));
        assert!(!is_unset_value(r"C:\Tools\ssh-mcp\config.toml"));
    }

    #[test]
    fn port_defaults_to_22() {
        let c = parse("[[hosts]]\nalias='a'\nhost='h'\nuser='u'\n", Path::new("t")).unwrap();
        assert_eq!(c.host("a").unwrap().port, 22);
    }
}
