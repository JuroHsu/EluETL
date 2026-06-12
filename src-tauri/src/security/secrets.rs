use serde::Deserialize;

/// 機密字串包裝：Debug / Display 一律遮罩，防止密碼經日誌或錯誤訊息外洩。
///
/// Week 2 將加上 keyring（OS keychain）持久化；目前僅作為 IPC 傳輸時的
/// 暫時性容器（測試連線由前端傳入，不落地）。
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// 取出明文。呼叫端責任：僅可傳給驅動程式，不得寫入日誌或錯誤訊息。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(***)")
    }
}

impl std::fmt::Display for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_leak() {
        let s = SecretString::new("p@ssw0rd".into());
        assert!(!format!("{s:?}").contains("p@ssw0rd"));
        assert!(!format!("{s}").contains("p@ssw0rd"));
        assert_eq!(s.expose(), "p@ssw0rd");
    }
}
