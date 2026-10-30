//! 產生遠端包裝 script，並解析輸出中的 cwd / 錯誤標記（PLAN §8.2）。
//!
//! 產生的 script（posix 模式）：
//! ```sh
//! export K='v'                                   # 1) env
//! cd -- '<remembered>' || { ERR=cwd; exit 254; } # 2) 記住的 cwd（若有）
//! cd -- '<override>'   || { ERR=cwd; exit 254; } #    本次指定的 cwd（若有）
//! __sshmcp_cwd=$(pwd)
//! { <pre_command>
//! } || { ERR=pre_command; exit 253; }            # 3) pre_command（若有）
//! cd -- "$__sshmcp_cwd"                          # 4) pre_command 可能 cd，回到原位
//! <user command>                                 # 5) 使用者命令（獨立成行）
//! __sshmcp_rc=$?                                 # 6) 回報 cwd 並保留 exit code
//! printf '\n<CWD 標記>%s\n' "$(pwd)"
//! exit $__sshmcp_rc
//! ```

use crate::config::{HostProfile, ShellMode};

pub const EXIT_CWD_FAILED: u32 = 254;
pub const EXIT_PRE_FAILED: u32 = 253;

/// 以 POSIX 單引號跳脫：`it's` → `'it'\''s'`
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str(r"'\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 產生每次呼叫唯一的 nonce（16 個 hex 字元）
pub fn new_nonce() -> String {
    format!("{:016x}", rand::random::<u64>())
}

fn cwd_marker(nonce: &str) -> String {
    format!("__SSHMCP_{nonce}_CWD=")
}

fn err_marker(nonce: &str) -> String {
    format!("__SSHMCP_{nonce}_ERR=")
}

/// 組出實際送到遠端執行的字串
///
/// - `remembered_cwd`：server 記住的目錄（或設定檔的 cwd）
/// - `override_cwd`：本次 `ssh_exec` 指定的 cwd（可為相對路徑，相對於 remembered）
pub fn build_command(
    profile: &HostProfile,
    remembered_cwd: Option<&str>,
    override_cwd: Option<&str>,
    command: &str,
    nonce: &str,
) -> String {
    if profile.shell_mode == ShellMode::Raw {
        return command.to_string();
    }
    let err = err_marker(nonce);
    let mut s = String::new();

    // 1) env
    for (k, v) in &profile.env {
        s.push_str(&format!("export {k}={}\n", shell_quote(v)));
    }
    // 2) cwd
    for dir in [remembered_cwd, override_cwd].into_iter().flatten() {
        s.push_str(&format!(
            "cd -- {} || {{ printf '{err}cwd\\n' >&2; exit {EXIT_CWD_FAILED}; }}\n",
            shell_quote(dir)
        ));
    }
    s.push_str("__sshmcp_cwd=$(pwd)\n");
    // 3) + 4) pre_command
    if let Some(pre) = &profile.pre_command {
        s.push_str("{ ");
        s.push_str(pre.trim_end());
        s.push_str(&format!(
            "\n}} || {{ printf '{err}pre_command\\n' >&2; exit {EXIT_PRE_FAILED}; }}\n"
        ));
        s.push_str("cd -- \"$__sshmcp_cwd\"\n");
    }
    // 5) 使用者命令獨立成行，結尾補換行，避免 `# 註解` 吃掉後面的程式
    s.push_str(command.trim_end_matches(['\n', '\r']));
    s.push('\n');
    // 6) 回報 cwd
    s.push_str("__sshmcp_rc=$?\n");
    s.push_str(&format!(
        "printf '\\n{}%s\\n' \"$(pwd)\"\n",
        cwd_marker(nonce)
    ));
    s.push_str("exit $__sshmcp_rc\n");
    s
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WrapError {
    Cwd,
    PreCommand,
}

#[derive(Debug)]
pub struct Parsed {
    pub stdout: String,
    pub stderr: String,
    pub new_cwd: Option<String>,
    pub wrap_error: Option<WrapError>,
}

/// 解析完整輸出：取出並移除 cwd 標記與錯誤標記。
/// 必須在截斷之前呼叫（否則標記行可能被截掉）。
pub fn parse_output(stdout: &str, stderr: &str, exit_code: Option<u32>, nonce: &str) -> Parsed {
    let cwd_m = cwd_marker(nonce);
    let err_m = err_marker(nonce);

    // ---- stdout：找最後一個 cwd 標記行 ----
    let mut new_cwd = None;
    let mut out = stdout.to_string();
    if let Some(pos) = out.rfind(&cwd_m) {
        let line_end = out[pos..].find('\n').map(|i| pos + i).unwrap_or(out.len());
        let value = out[pos + cwd_m.len()..line_end]
            .trim_end_matches('\r')
            .to_string();
        if !value.is_empty() {
            new_cwd = Some(value);
        }
        // 移除標記行，以及 printf 在它之前補上的那個 '\n'
        let mut start = pos;
        if start > 0 && out.as_bytes()[start - 1] == b'\n' {
            start -= 1;
        }
        let mut end = line_end;
        if end < out.len() {
            end += 1; // 標記行結尾的 '\n'
        }
        // 若移除的是開頭的換行，且後面還有內容，保留一個換行
        out.replace_range(start..end, "");
    }

    // ---- stderr：錯誤標記 ----
    let mut wrap_error = None;
    let mut err_lines = Vec::new();
    for line in stderr.split_inclusive('\n') {
        if let Some(rest) = line.trim_end().strip_prefix(&err_m) {
            let kind = match rest {
                "cwd" => Some(WrapError::Cwd),
                "pre_command" => Some(WrapError::PreCommand),
                _ => None,
            };
            // 只有 exit code 也吻合時才視為 server 包裝錯誤
            wrap_error = match (kind, exit_code) {
                (Some(WrapError::Cwd), Some(EXIT_CWD_FAILED)) => Some(WrapError::Cwd),
                (Some(WrapError::PreCommand), Some(EXIT_PRE_FAILED)) => Some(WrapError::PreCommand),
                _ => wrap_error,
            };
            continue;
        }
        err_lines.push(line);
    }

    Parsed {
        stdout: out,
        stderr: err_lines.concat(),
        new_cwd,
        wrap_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn profile() -> HostProfile {
        let mut env = BTreeMap::new();
        env.insert("LANG".into(), "C.UTF-8".into());
        env.insert("Q".into(), "it's $HOME".into());
        HostProfile {
            alias: "a".into(),
            description: String::new(),
            host: "h".into(),
            port: 22,
            user: "u".into(),
            connect_timeout_s: 10,
            keepalive_s: 30,
            cwd: Some("/srv".into()),
            pre_command: Some("source /opt/env.sh".into()),
            env,
            shell_mode: ShellMode::Posix,
            allow_dangerous: false,
        }
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc"), "'abc'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$X\n`y`"), "'$X\n`y`'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn build_contains_all_steps_in_order() {
        let s = build_command(
            &profile(),
            Some("/srv/a b"),
            Some("sub"),
            "make # done",
            "N",
        );
        let idx = |needle: &str| {
            s.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}\n{s}"))
        };
        assert!(idx("export LANG='C.UTF-8'") < idx("cd -- '/srv/a b'"));
        assert!(s.contains(r"export Q='it'\''s $HOME'"));
        assert!(idx("cd -- '/srv/a b'") < idx("cd -- 'sub'"));
        assert!(idx("cd -- 'sub'") < idx("source /opt/env.sh"));
        assert!(idx("source /opt/env.sh") < idx("cd -- \"$__sshmcp_cwd\""));
        assert!(idx("cd -- \"$__sshmcp_cwd\"") < idx("make # done\n"));
        assert!(idx("make # done\n") < idx("__sshmcp_rc=$?"));
        assert!(s.contains("__SSHMCP_N_CWD="));
    }

    #[test]
    fn raw_mode_passthrough() {
        let mut p = profile();
        p.shell_mode = ShellMode::Raw;
        assert_eq!(build_command(&p, Some("/x"), None, "dir", "N"), "dir");
    }

    #[test]
    fn parse_strips_marker() {
        let out = "hello\nworld\n\n__SSHMCP_N_CWD=/srv/x\n";
        let p = parse_output(out, "", Some(0), "N");
        assert_eq!(p.stdout, "hello\nworld\n");
        assert_eq!(p.new_cwd.as_deref(), Some("/srv/x"));
        assert!(p.wrap_error.is_none());
    }

    #[test]
    fn parse_no_trailing_newline_output() {
        // 命令輸出沒有換行結尾（例如 printf abc）
        let out = "abc\n__SSHMCP_N_CWD=/tmp\n";
        let p = parse_output(out, "", Some(0), "N");
        assert_eq!(p.stdout, "abc");
        assert_eq!(p.new_cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn parse_missing_marker_keeps_cwd_unknown() {
        let p = parse_output("x\n", "", Some(0), "N");
        assert_eq!(p.stdout, "x\n");
        assert!(p.new_cwd.is_none());
    }

    #[test]
    fn parse_wrap_errors_need_matching_exit_code() {
        let p = parse_output(
            "",
            "__SSHMCP_N_ERR=cwd\nbash: cd: no such dir\n",
            Some(254),
            "N",
        );
        assert_eq!(p.wrap_error, Some(WrapError::Cwd));
        assert_eq!(p.stderr, "bash: cd: no such dir\n");

        let p = parse_output("", "__SSHMCP_N_ERR=pre_command\n", Some(253), "N");
        assert_eq!(p.wrap_error, Some(WrapError::PreCommand));

        // 使用者命令自己回傳 254，沒有標記 → 不是 wrap 錯誤
        let p = parse_output("", "oops\n", Some(254), "N");
        assert!(p.wrap_error.is_none());
        // 別的 nonce 的標記不算
        let p = parse_output("", "__SSHMCP_OTHER_ERR=cwd\n", Some(254), "N");
        assert!(p.wrap_error.is_none());
    }
}
