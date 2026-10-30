//! 在既有 SSH 連線上開 channel 執行一個命令，收集 stdout / stderr / exit status。

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use russh::ChannelMsg;
use russh::client::Handle;

use super::client::Client;

#[derive(Debug, Default)]
pub struct RawOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<u32>,
    pub exit_signal: Option<String>,
    pub timed_out: bool,
    pub duration_ms: u128,
}

/// 執行命令並等到 channel 關閉（或逾時）
///
/// 注意：`ExitStatus` 可能比最後一筆 `Data` 先到，所以一律等到 `Close`
/// （或 channel 結束）才算完成。
pub async fn run(handle: &Handle<Client>, command: &str, timeout: Duration) -> Result<RawOutput> {
    let started = Instant::now();
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| anyhow!("開啟 SSH channel 失敗：{e}"))?;
    channel
        .exec(true, command.as_bytes().to_vec())
        .await
        .map_err(|e| anyhow!("送出命令失敗：{e}"))?;
    // 不提供 stdin：立即送 EOF，避免讀 stdin 的命令卡住
    let _ = channel.eof().await;

    let mut out = RawOutput::default();
    let read_loop = async {
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext } => {
                    if ext == 1 {
                        out.stderr.extend_from_slice(&data);
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => out.exit_code = Some(exit_status),
                ChannelMsg::ExitSignal {
                    signal_name,
                    error_message,
                    ..
                } => {
                    let mut s = format!("{signal_name:?}");
                    if !error_message.is_empty() {
                        s.push_str(&format!(" ({error_message})"));
                    }
                    out.exit_signal = Some(s);
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
    };

    if tokio::time::timeout(timeout, read_loop).await.is_err() {
        out.timed_out = true;
        // 關閉 channel；遠端行程不一定會被終止（PLAN §8.2）
        let _ = channel.close().await;
    }
    out.duration_ms = started.elapsed().as_millis();
    Ok(out)
}
