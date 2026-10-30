//! 稽核記錄：每次工具呼叫寫一行 JSON 到 `<log_dir>/audit-YYYYMMDD.jsonl`（PLAN §6 L8）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;

#[derive(Debug, Serialize, Default)]
pub struct AuditRecord<'a> {
    pub ts: String,
    pub tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u128>,
    pub blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_rule: Option<&'a str>,
    pub dangerous_allowed: bool,
    pub timed_out: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a str>,
}

pub struct Audit {
    dir: PathBuf,
    lock: Mutex<()>,
}

pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

impl Audit {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    /// 寫入失敗只記 log，不影響工具執行
    pub fn write(&self, rec: &AuditRecord<'_>) {
        let _g = self.lock.lock();
        let file = self.dir.join(format!(
            "audit-{}.jsonl",
            chrono::Local::now().format("%Y%m%d")
        ));
        let line = match serde_json::to_string(rec) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("稽核記錄序列化失敗：{e}");
                return;
            }
        };
        let res = std::fs::create_dir_all(&self.dir).and_then(|_| {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)?;
            writeln!(f, "{line}")
        });
        if let Err(e) = res {
            tracing::error!("寫入稽核記錄失敗（{}）：{e}", file.display());
        }
    }
}
