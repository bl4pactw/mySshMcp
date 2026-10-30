//! SSH 連線池：依 host alias 重用連線、閒置回收、斷線重連、限制同時 channel 數（PLAN §8.3）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use russh::Disconnect;
use russh::client::Handle;
use tokio::sync::{Mutex, Semaphore};

use super::client::{self, Client};
use crate::config::{General, HostProfile};

pub struct Session {
    pub handle: Handle<Client>,
    /// 限制此主機同時開啟的 channel 數（避免碰到 sshd MaxSessions）
    pub channels: Semaphore,
    last_used: std::sync::Mutex<Instant>,
}

impl Session {
    fn touch(&self) {
        if let Ok(mut t) = self.last_used.lock() {
            *t = Instant::now();
        }
    }

    fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }
}

type Slot = Arc<Mutex<Option<Arc<Session>>>>;

pub struct Pool {
    general: General,
    /// 每個 alias 一個 slot（各自上鎖），連線慢的主機不會擋住其他主機
    slots: std::sync::Mutex<HashMap<String, Slot>>,
}

impl Pool {
    pub fn new(general: General) -> Arc<Self> {
        Arc::new(Self {
            general,
            slots: std::sync::Mutex::new(HashMap::new()),
        })
    }

    fn slot(&self, alias: &str) -> Slot {
        let mut m = self.slots.lock().expect("pool mutex poisoned");
        m.entry(alias.to_string()).or_default().clone()
    }

    /// 取得可用連線：已存在且未關閉就重用，否則重新連線
    pub async fn get(&self, p: &HostProfile) -> Result<Arc<Session>> {
        let slot = self.slot(&p.alias);
        let mut guard = slot.lock().await;
        if let Some(s) = guard.as_ref() {
            if !s.handle.is_closed() {
                s.touch();
                return Ok(s.clone());
            }
            tracing::info!(alias = %p.alias, "連線已中斷，重新連線");
            *guard = None;
        }
        let handle = client::connect(&self.general, p).await?;
        let s = Arc::new(Session {
            handle,
            channels: Semaphore::new(self.general.max_channels_per_host),
            last_used: std::sync::Mutex::new(Instant::now()),
        });
        *guard = Some(s.clone());
        Ok(s)
    }

    /// 連線失效時由呼叫端移除，下次 get 會重連
    pub async fn invalidate(&self, alias: &str) {
        let slot = self.slot(alias);
        let old = slot.lock().await.take();
        if let Some(s) = old {
            let _ = s
                .handle
                .disconnect(Disconnect::ByApplication, "reconnect", "en")
                .await;
        }
    }

    /// 回收閒置連線（正在使用中的連線不回收）
    pub async fn reap_idle(&self) {
        let ttl = Duration::from_secs(self.general.idle_session_ttl_s);
        let slots: Vec<(String, Slot)> = {
            let m = self.slots.lock().expect("pool mutex poisoned");
            m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        for (alias, slot) in slots {
            let Ok(mut guard) = slot.try_lock() else {
                continue;
            };
            let expired = guard.as_ref().is_some_and(|s| {
                s.handle.is_closed()
                    || (s.idle_for() > ttl
                        && s.channels.available_permits() == self.general.max_channels_per_host)
            });
            if expired && let Some(s) = guard.take() {
                tracing::info!(%alias, "回收閒置連線");
                let _ = s
                    .handle
                    .disconnect(Disconnect::ByApplication, "idle", "en")
                    .await;
            }
        }
    }

    /// 啟動背景回收工作
    pub fn spawn_reaper(self: &Arc<Self>) {
        let pool = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tick.tick().await;
                match pool.upgrade() {
                    Some(p) => p.reap_idle().await,
                    None => break,
                }
            }
        });
    }
}
