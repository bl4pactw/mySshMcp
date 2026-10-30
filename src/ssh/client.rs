//! russh 連線：主機金鑰驗證（known_hosts）與 ssh-agent 認證（PLAN §8.4、§8.5）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use russh::Disconnect;
use russh::client::{self, Handle};
use russh::keys::PublicKeyOrCertificate;
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PublicKey};

use crate::config::{AgentKind, General, HostProfile};

/// russh client handler：只負責主機金鑰驗證
pub struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    /// 拒絕連線時的原因，供上層組成清楚的錯誤訊息
    reject_reason: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                self.reject(
                    "主機使用 SSH 憑證（host certificate），v0.1 不支援；請改用一般主機金鑰".into(),
                );
                return Ok(false);
            }
        };
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => {
                let fp = key.fingerprint(HashAlg::Sha256);
                self.reject(format!(
                    "known_hosts（{}）中沒有 {} 的主機金鑰（{} {}）。請先用 ssh 手動連線一次確認並記錄金鑰：ssh -p {} <user>@{}",
                    self.known_hosts.display(),
                    host_label(&self.host, self.port),
                    key.algorithm().as_str(),
                    fp,
                    self.port,
                    self.host
                ));
                Ok(false)
            }
            Err(russh::keys::Error::KeyChanged { line }) => {
                self.reject(format!(
                    "⚠ {} 的主機金鑰與 known_hosts 第 {line} 行不符，可能是主機重灌或遭到中間人攻擊。已拒絕連線；確認無誤後請自行更新 known_hosts（ssh-keygen -R）",
                    host_label(&self.host, self.port)
                ));
                Ok(false)
            }
            Err(e) => {
                self.reject(format!(
                    "無法讀取 known_hosts（{}）：{e}",
                    self.known_hosts.display()
                ));
                Ok(false)
            }
        }
    }
}

impl Client {
    fn reject(&self, reason: String) {
        if let Ok(mut r) = self.reject_reason.lock() {
            *r = Some(reason);
        }
    }
}

fn host_label(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

// ---------------------------------------------------------------------------
// ssh-agent
// ---------------------------------------------------------------------------

/// 統一不同平台 agent 連線的型別
pub enum Agent {
    #[cfg(unix)]
    Unix(AgentClient<tokio::net::UnixStream>),
    #[cfg(windows)]
    Pipe(AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>),
    #[cfg(windows)]
    Pageant(AgentClient<pageant::PageantStream>),
}

impl Agent {
    pub async fn connect(general: &General) -> Result<Agent> {
        match general.agent {
            #[cfg(unix)]
            AgentKind::Env => Ok(Agent::Unix(AgentClient::connect_env().await.map_err(|e| {
                anyhow!("無法連線 ssh-agent（SSH_AUTH_SOCK）：{e}")
            })?)),
            #[cfg(windows)]
            AgentKind::OpenSsh => Ok(Agent::Pipe(
                AgentClient::connect_named_pipe(&general.agent_pipe)
                    .await
                    .map_err(|e| {
                        anyhow!(
                            "無法連線 Windows OpenSSH Agent（{}）：{e}。請確認 ssh-agent 服務已啟動（Start-Service ssh-agent）",
                            general.agent_pipe
                        )
                    })?,
            )),
            #[cfg(windows)]
            AgentKind::Pageant => Ok(Agent::Pageant(
                AgentClient::connect_pageant()
                    .await
                    .map_err(|e| anyhow!("無法連線 Pageant：{e}。請確認 Pageant 已執行並載入金鑰"))?,
            )),
            #[allow(unreachable_patterns)]
            other => bail!("此平台不支援 agent 類型 {other:?}"),
        }
    }

    pub async fn identities(&mut self) -> Result<Vec<AgentIdentity>> {
        let r = match self {
            #[cfg(unix)]
            Agent::Unix(a) => a.request_identities().await,
            #[cfg(windows)]
            Agent::Pipe(a) => a.request_identities().await,
            #[cfg(windows)]
            Agent::Pageant(a) => a.request_identities().await,
        };
        r.map_err(|e| anyhow!("讀取 ssh-agent 金鑰清單失敗：{e}"))
    }

    async fn auth(
        &mut self,
        handle: &mut Handle<Client>,
        user: &str,
        key: PublicKey,
        hash: Option<HashAlg>,
    ) -> Result<bool> {
        let r = match self {
            #[cfg(unix)]
            Agent::Unix(a) => handle.authenticate_publickey_with(user, key, hash, a).await,
            #[cfg(windows)]
            Agent::Pipe(a) => handle.authenticate_publickey_with(user, key, hash, a).await,
            #[cfg(windows)]
            Agent::Pageant(a) => handle.authenticate_publickey_with(user, key, hash, a).await,
        };
        match r {
            Ok(res) => Ok(res.success()),
            Err(e) => {
                tracing::debug!("agent 簽章失敗：{e}");
                Ok(false)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 建立連線
// ---------------------------------------------------------------------------

/// 建立已認證的 SSH 連線
pub async fn connect(general: &General, p: &HostProfile) -> Result<Handle<Client>> {
    let reject_reason = Arc::new(Mutex::new(None));
    let handler = Client {
        host: p.host.clone(),
        port: p.port,
        known_hosts: general.known_hosts.clone(),
        reject_reason: reject_reason.clone(),
    };
    let config = Arc::new(client::Config {
        keepalive_interval: (p.keepalive_s > 0).then(|| Duration::from_secs(p.keepalive_s)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });

    let target = (p.host.as_str(), p.port);
    let connect = client::connect(config, target, handler);
    let mut handle =
        match tokio::time::timeout(Duration::from_secs(p.connect_timeout_s), connect).await {
            Err(_) => bail!(
                "連線 {}（{}）逾時（{} 秒）",
                p.alias,
                host_label(&p.host, p.port),
                p.connect_timeout_s
            ),
            Ok(Err(e)) => {
                if let Some(reason) = reject_reason.lock().ok().and_then(|r| r.clone()) {
                    bail!("{reason}");
                }
                bail!(
                    "連線 {}（{}）失敗：{e}",
                    p.alias,
                    host_label(&p.host, p.port)
                );
            }
            Ok(Ok(h)) => h,
        };

    // ---- ssh-agent 認證：逐一嘗試 agent 中的金鑰 ----
    let mut agent = Agent::connect(general).await?;
    let identities = agent.identities().await?;
    if identities.is_empty() {
        bail!("ssh-agent 中沒有可用金鑰，請先執行 ssh-add 載入金鑰");
    }
    // RSA 金鑰需要選擇伺服器支援的雜湊演算法
    let rsa_hash = handle
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();
    for id in identities {
        let key = id.public_key().into_owned();
        let hash = if key.algorithm().is_rsa() {
            rsa_hash
        } else {
            None
        };
        if agent.auth(&mut handle, &p.user, key, hash).await? {
            tracing::info!(alias = %p.alias, "SSH 認證成功（{}）", id.comment());
            return Ok(handle);
        }
    }
    let _ = handle
        .disconnect(Disconnect::ByApplication, "auth failed", "en")
        .await;
    bail!(
        "以帳號「{}」登入 {} 失敗：ssh-agent 中的金鑰都不被接受。請確認公鑰已加入遠端 ~/.ssh/authorized_keys",
        p.user,
        host_label(&p.host, p.port)
    )
}
