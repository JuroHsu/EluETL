use serde::ser::SerializeStruct;

/// 統一錯誤型別：所有 IPC command 以此回傳前端。
/// 安全約束：錯誤訊息不得包含連線字串、密碼等機密（見 `security::secrets`）。
#[derive(Debug, thiserror::Error)]
pub enum EluEtlError {
    #[error("資料庫錯誤: {0}")]
    Db(String),

    #[error("I/O 錯誤: {0}")]
    Io(#[from] std::io::Error),

    #[error("設定錯誤: {0}")]
    Config(String),

    #[error("找不到資源: {0}")]
    NotFound(String),

    #[error("檔案解析錯誤: {0}")]
    Excel(String),

    #[error("ETL 錯誤: {0}")]
    Etl(String),

    #[error("機密儲存錯誤: {0}")]
    Secret(String),

    #[error("操作已取消")]
    Cancelled,

    #[error("尚未實作: {0}")]
    NotImplemented(&'static str),
}

impl EluEtlError {
    /// 穩定的錯誤代碼，供前端依代碼分流處理（訊息文字可能隨版本變動）。
    pub fn code(&self) -> &'static str {
        match self {
            EluEtlError::Db(_) => "DB_ERROR",
            EluEtlError::Io(_) => "IO_ERROR",
            EluEtlError::Config(_) => "CONFIG_ERROR",
            EluEtlError::NotFound(_) => "NOT_FOUND",
            EluEtlError::Excel(_) => "FILE_ERROR",
            EluEtlError::Etl(_) => "ETL_ERROR",
            EluEtlError::Secret(_) => "SECRET_ERROR",
            EluEtlError::Cancelled => "CANCELLED",
            EluEtlError::NotImplemented(_) => "NOT_IMPLEMENTED",
        }
    }
}

impl From<calamine::Error> for EluEtlError {
    fn from(e: calamine::Error) -> Self {
        EluEtlError::Excel(e.to_string())
    }
}

impl From<rust_xlsxwriter::XlsxError> for EluEtlError {
    fn from(e: rust_xlsxwriter::XlsxError) -> Self {
        EluEtlError::Excel(e.to_string())
    }
}

impl From<csv::Error> for EluEtlError {
    fn from(e: csv::Error) -> Self {
        EluEtlError::Excel(e.to_string())
    }
}

impl From<keyring_core::Error> for EluEtlError {
    fn from(e: keyring_core::Error) -> Self {
        EluEtlError::Secret(e.to_string())
    }
}

impl From<serde_json::Error> for EluEtlError {
    fn from(e: serde_json::Error) -> Self {
        EluEtlError::Config(e.to_string())
    }
}

impl From<tokio::task::JoinError> for EluEtlError {
    fn from(e: tokio::task::JoinError) -> Self {
        EluEtlError::Etl(format!("背景工作失敗: {e}"))
    }
}

impl From<sqlx::Error> for EluEtlError {
    fn from(e: sqlx::Error) -> Self {
        EluEtlError::Db(e.to_string())
    }
}

impl From<tiberius::error::Error> for EluEtlError {
    fn from(e: tiberius::error::Error) -> Self {
        EluEtlError::Db(e.to_string())
    }
}

impl serde::Serialize for EluEtlError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("EluEtlError", 2)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_code_and_message() {
        let err = EluEtlError::Config("缺少 host".into());
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], "CONFIG_ERROR");
        assert_eq!(json["message"], "設定錯誤: 缺少 host");
    }
}
