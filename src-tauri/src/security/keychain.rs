use keyring_core::{Entry, Error};
use uuid::Uuid;

use crate::models::errors::EluEtlError;
use crate::security::secrets::SecretString;

const SERVICE: &str = "com.elu.etl";

/// 選擇平台原生 keystore（macOS Keychain / Windows Credential Manager /
/// Linux Secret Service）。App 啟動時呼叫一次。
pub fn init() -> Result<(), EluEtlError> {
    // Linux 傳 true：使用 Secret Service（持久化），而非 kernel keyutils（重開機即失）
    keyring::use_native_store(true)?;
    Ok(())
}

/// 密碼存 OS keychain，account 為 ConnectionId。
/// 注意：keyring 為同步 API，呼叫端應以 spawn_blocking 包裝。
pub fn save_password(conn_id: &Uuid, secret: &SecretString) -> Result<(), EluEtlError> {
    Entry::new(SERVICE, &conn_id.to_string())?.set_password(secret.expose())?;
    Ok(())
}

pub fn load_password(conn_id: &Uuid) -> Result<Option<SecretString>, EluEtlError> {
    match Entry::new(SERVICE, &conn_id.to_string())?.get_password() {
        Ok(p) => Ok(Some(SecretString::new(p))),
        Err(Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete_password(conn_id: &Uuid) -> Result<(), EluEtlError> {
    match Entry::new(SERVICE, &conn_id.to_string())?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
