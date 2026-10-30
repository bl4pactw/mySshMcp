//! 輸出截斷與完整輸出存檔（PLAN §8.6）。

use std::path::{Path, PathBuf};

/// 超過 `max` bytes 時保留頭 60%、尾 40%，中間插入截斷說明。
/// 回傳 (結果, 是否截斷)。切點會對齊 UTF-8 字元邊界。
pub fn truncate(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let head_len = floor_char_boundary(s, max * 6 / 10);
    let tail_start = ceil_char_boundary(s, s.len() - (max - max * 6 / 10));
    let omitted = tail_start - head_len;
    (
        format!(
            "{}\n... [truncated {omitted} bytes] ...\n{}",
            &s[..head_len],
            &s[tail_start..]
        ),
        true,
    )
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// 將完整輸出存到 `<dir>/<timestamp>-<alias>.log`，回傳路徑
pub fn save_full(
    dir: &Path,
    alias: &str,
    command: &str,
    stdout: &str,
    stderr: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = format!(
        "{}-{}-{:04x}.log",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        alias,
        rand::random::<u16>()
    );
    let path = dir.join(name);
    let body = format!(
        "# host: {alias}\n# command:\n{command}\n\n=== STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}\n"
    );
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_is_untouched() {
        assert_eq!(truncate("abc", 10), ("abc".to_string(), false));
    }

    #[test]
    fn long_keeps_head_and_tail() {
        let s: String = (0..1000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let (t, cut) = truncate(&s, 100);
        assert!(cut);
        assert!(t.starts_with(&s[..60]));
        assert!(t.ends_with(&s[960..]));
        assert!(t.contains("[truncated 900 bytes]"));
    }

    #[test]
    fn utf8_boundaries() {
        let s = "中文測試".repeat(100); // 每字 3 bytes
        let (t, cut) = truncate(&s, 101);
        assert!(cut);
        assert!(t.is_char_boundary(t.len()));
    }
}
