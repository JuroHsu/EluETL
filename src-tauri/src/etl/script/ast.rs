//! ETL 腳本 DSL 的抽象語法樹。
//!
//! 語法範例（每段陳述式以 GO 分隔）：
//! ```text
//! If [FILENAME].[SHEET1].email == [EluAdminCenter].[dbo].[Account].email
//! [EluAdminCenter].[dbo].[ExternalIdentityMappings] ADD
//! {
//!  AccountId = [EluAdminCenter].[dbo].[Account].Id
//! ,ExternalId = [FILENAME].[SHEET1].Id
//! ,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
//! }
//! GO
//! ```
//! 語意：來源檔每一行，以 email 比對 DB 資料表 Account（hash lookup，
//! 文字不分大小寫），命中者組裝欄位後 INSERT 到目標表；未命中進錯誤報告。

/// 欄位參照：`prefix.column`，prefix 為 0..3 段（識別來源檔或 DB 資料表）。
#[derive(Debug, Clone, PartialEq)]
pub struct ColRef {
    pub prefix: Vec<String>,
    pub column: String,
    pub line: usize,
}

impl ColRef {
    /// 正規化 prefix（小寫、以 . 連接），供來源 / lookup 歸屬比對。
    pub fn prefix_key(&self) -> String {
        self.prefix
            .iter()
            .map(|p| p.to_lowercase())
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// 指派值：欄位參照或字面值。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Col(ColRef),
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub target_column: String,
    pub value: Expr,
    pub line: usize,
}

/// `IF <source>.<col> == <table>.<col>` 比對條件。
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub left: ColRef,
    pub right: ColRef,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub condition: Option<Condition>,
    /// 目標資料表（1..3 段：[db].[schema].[table]）
    pub target_table: Vec<String>,
    pub assignments: Vec<Assignment>,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub statements: Vec<Statement>,
}

/// 解析 / 驗證錯誤（含行號，供編輯器標示）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptIssue {
    pub line: usize,
    pub message: String,
}
