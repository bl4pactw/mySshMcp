//! 每個 host alias 記住的工作目錄（只存在記憶體，server 重啟後回到設定檔的 cwd）。

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct CwdState {
    map: Mutex<HashMap<String, String>>,
}

impl CwdState {
    /// 目前記住的目錄；沒有記錄時回傳設定檔的 cwd
    pub fn get(&self, alias: &str, config_cwd: Option<&str>) -> Option<String> {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.get(alias).cloned())
            .or_else(|| config_cwd.map(str::to_string))
    }

    /// 更新記住的目錄（多個 exec 同時執行時，以最後完成者為準）
    pub fn set(&self, alias: &str, cwd: String) {
        if let Ok(mut m) = self.map.lock() {
            m.insert(alias.to_string(), cwd);
        }
    }

    /// 清除記錄，回到設定檔的 cwd
    pub fn reset(&self, alias: &str) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(alias);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_config() {
        let s = CwdState::default();
        assert_eq!(s.get("a", Some("/cfg")).as_deref(), Some("/cfg"));
        assert_eq!(s.get("a", None), None);
        s.set("a", "/tmp".into());
        assert_eq!(s.get("a", Some("/cfg")).as_deref(), Some("/tmp"));
        s.reset("a");
        assert_eq!(s.get("a", Some("/cfg")).as_deref(), Some("/cfg"));
    }
}
