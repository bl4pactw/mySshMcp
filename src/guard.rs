//! 命令黑名單（PLAN §6 L5）。
//!
//! 只是第二道防線：黑名單一定能被繞過（base64、變數展開、`sh -c` 等），
//! 主要防線是遠端低權限帳號。

use regex::Regex;

/// 允許危險命令的環境變數（由使用者在 MCP 設定中設定，LLM 無法自行開啟）
pub const ALLOW_DANGEROUS_ENV: &str = "SSH_MCP_ALLOW_DANGEROUS";

/// 環境變數旗標：只有 "1" / "true"（不分大小寫）視為開啟
pub fn env_flag(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true")
}

pub struct Guard {
    rules: Vec<(String, Regex)>,
    env_allows: bool,
}

pub enum Verdict {
    Allowed,
    /// 命中規則，但主機與環境變數都允許 → 放行並記錄
    AllowedDangerous(String),
    Blocked(String),
}

impl Guard {
    pub fn new(patterns: &[String]) -> anyhow::Result<Self> {
        let rules = patterns
            .iter()
            .map(|p| Ok((p.clone(), Regex::new(p)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        // "1" 或 "true"（.mcpb 的 boolean user_config 會傳入 "true" / "false"）
        let env_allows = std::env::var(ALLOW_DANGEROUS_ENV)
            .map(|v| env_flag(&v))
            .unwrap_or(false);
        Ok(Self { rules, env_allows })
    }

    #[cfg(test)]
    fn with_env(patterns: &[String], env_allows: bool) -> Self {
        let mut g = Self::new(patterns).expect("valid patterns");
        g.env_allows = env_allows;
        g
    }

    pub fn check(&self, command: &str, host_allows: bool) -> Verdict {
        for (src, re) in &self.rules {
            if re.is_match(command) {
                return if host_allows && self.env_allows {
                    Verdict::AllowedDangerous(src.clone())
                } else {
                    Verdict::Blocked(src.clone())
                };
            }
        }
        Verdict::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_deny_patterns;

    fn blocked(g: &Guard, cmd: &str) -> bool {
        matches!(g.check(cmd, false), Verdict::Blocked(_))
    }

    #[test]
    fn default_rules() {
        let g = Guard::with_env(&default_deny_patterns(), false);
        for bad in [
            "rm -rf /",
            "sudo rm -rf / ",
            "cd /tmp && rm -fr /*",
            "mkfs.ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda bs=1M",
            "reboot",
            "sudo shutdown -h now",
            ":(){ :|:& };:",
        ] {
            assert!(blocked(&g, bad), "should block: {bad}");
        }
        for ok in [
            "rm -rf ./build",
            "rm -rf /tmp/foo",
            "ls -la /",
            "dd if=a of=b",
            "echo rebooting-check-done",
            "make -j8",
        ] {
            assert!(!blocked(&g, ok), "should allow: {ok}");
        }
    }

    #[test]
    fn env_flag_values() {
        assert!(env_flag("1") && env_flag("true") && env_flag("TRUE"));
        assert!(
            !env_flag("false")
                && !env_flag("0")
                && !env_flag("")
                && !env_flag("${user_config.allow_dangerous}")
        );
    }

    #[test]
    fn needs_both_host_and_env() {
        let pats = default_deny_patterns();
        assert!(matches!(
            Guard::with_env(&pats, false).check("reboot", true),
            Verdict::Blocked(_)
        ));
        assert!(matches!(
            Guard::with_env(&pats, true).check("reboot", false),
            Verdict::Blocked(_)
        ));
        assert!(matches!(
            Guard::with_env(&pats, true).check("reboot", true),
            Verdict::AllowedDangerous(_)
        ));
    }
}
