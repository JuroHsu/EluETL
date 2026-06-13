//! ETL 腳本 DSL 的抽象語法樹。
//!
//! 語法範例（工作項目以 `WORK '名稱' { … }` 包裹；舊式 GO 分隔仍相容）：
//! ```text
//! WORK 'EluCloudAccount綁定EnterId' {
//! If [SOURCE].email == [dbo].[Account].email
//! [dbo].[ExternalIdentityMappings] ADD
//! {
//!  AccountId = [dbo].[Account].Id
//! ,ExternalId = [SOURCE].Id
//! ,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
//! }
//! }
//! GO
//! ```
//! 語意：來源每一行，以 email 比對 DB 資料表 Account（hash lookup，
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

/// 產生器（`Gen.XXX` / `Gen.XXX(Text)`）：每列執行時產生值。
/// `…Text` 變體輸出文字表示（給 nvarchar 類欄位）；雜湊類以來源整列內容計算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenKind {
    /// UUID v4（給 uniqueidentifier / uuid 欄位；以字串繫結，DB 端隱含轉換）
    Guid,
    GuidText,
    /// ULID（26 字元 Crockford base32，時間排序友善）
    Ulid,
    /// 執行當下日期 / 時間（同一次執行內所有列相同）
    Date,
    DateText,
    DateTime,
    DateTimeText,
    /// 含時區位移的 ISO 8601 文字（給 datetimeoffset 類欄位）
    DateTimeOffset,
    DateTimeOffsetText,
    /// 來源整列雜湊（hex 小寫；欄位以 US 分隔符串接後計算）
    Sha256,
    Sha512,
    Md5,
}

impl GenKind {
    /// 解析產生器名稱（不分大小寫）；`text_variant` = 帶 `(Text)` 後綴。
    pub fn parse(name: &str, text_variant: bool) -> Option<GenKind> {
        match (name.to_uppercase().as_str(), text_variant) {
            ("GUID", false) => Some(GenKind::Guid),
            ("GUID", true) => Some(GenKind::GuidText),
            ("ULID", false) => Some(GenKind::Ulid),
            ("DATE", false) => Some(GenKind::Date),
            ("DATE", true) => Some(GenKind::DateText),
            ("DATETIME", false) => Some(GenKind::DateTime),
            ("DATETIME", true) => Some(GenKind::DateTimeText),
            ("DATETIMEOFFSET", false) => Some(GenKind::DateTimeOffset),
            ("DATETIMEOFFSET", true) => Some(GenKind::DateTimeOffsetText),
            ("SHA256", false) => Some(GenKind::Sha256),
            ("SHA512", false) => Some(GenKind::Sha512),
            ("MD5", false) => Some(GenKind::Md5),
            _ => None,
        }
    }

    /// 正規名稱（UI 下拉與腳本產生用，如 `GUID(Text)`）。
    pub fn label(&self) -> &'static str {
        match self {
            GenKind::Guid => "GUID",
            GenKind::GuidText => "GUID(Text)",
            GenKind::Ulid => "ULID",
            GenKind::Date => "Date",
            GenKind::DateText => "Date(Text)",
            GenKind::DateTime => "DateTime",
            GenKind::DateTimeText => "DateTime(Text)",
            GenKind::DateTimeOffset => "DateTimeOffset",
            GenKind::DateTimeOffsetText => "DateTimeOffset(Text)",
            GenKind::Sha256 => "SHA256",
            GenKind::Sha512 => "SHA512",
            GenKind::Md5 => "MD5",
        }
    }

    pub const ALL_LABELS: &'static str = "GUID、GUID(Text)、ULID、Date、DateTime、DateTimeOffset、\
         Date(Text)、DateTime(Text)、DateTimeOffset(Text)、SHA256、SHA512、MD5";
}

/// 指派值：欄位參照、字面值、產生器，或 `+` 合成欄位。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Col(ColRef),
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    Gen(GenKind),
    /// 合成欄位：各項轉為文字後串接（NULL 視為空字串），如
    /// `N'MICROSOFT_ENTRA_ID:' + [dbo].[DirectoryAccounts].[DisplayName]`
    Concat(Vec<Expr>),
}

impl Expr {
    /// 走訪運算式中的所有欄位參照（含合成欄位的巢狀項）。
    pub fn col_refs(&self) -> Vec<&ColRef> {
        match self {
            Expr::Col(r) => vec![r],
            Expr::Concat(parts) => parts.iter().flat_map(Expr::col_refs).collect(),
            _ => Vec::new(),
        }
    }

    /// 正規 DSL 文字（視覺編輯器顯示 / 腳本產生用）。
    pub fn to_dsl(&self) -> String {
        match self {
            Expr::Col(r) => {
                let mut parts: Vec<String> = r.prefix.iter().map(|p| format!("[{p}]")).collect();
                parts.push(format!("[{}]", r.column));
                parts.join(".")
            }
            Expr::Text(s) => format!("N'{}'", s.replace('\'', "''")),
            Expr::Int(v) => v.to_string(),
            Expr::Float(v) => v.to_string(),
            Expr::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
            Expr::Null => "NULL".to_string(),
            Expr::Gen(k) => format!("Gen.{}", k.label()),
            Expr::Concat(parts) => parts
                .iter()
                .map(Expr::to_dsl)
                .collect::<Vec<_>>()
                .join(" + "),
        }
    }
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
    /// 工作項目名稱（`WORK '名稱' { … }`；舊式 GO 分隔的裸陳述式為 None）
    pub name: Option<String>,
    pub condition: Option<Condition>,
    /// 目標資料表（1..3 段：[db].[schema].[table]）
    pub target_table: Vec<String>,
    pub assignments: Vec<Assignment>,
    pub line: usize,
}

/// SOURCE 宣告：inline 檔案參數，或以名稱引用已儲存的連線
/// （檔案連線直接引用；資料庫連線需附 TABLE='…' 或 QUERY='…' 指明讀什麼）。
#[derive(Debug, Clone, PartialEq)]
pub enum SourceDecl {
    File(FileSource),
    Connection(ConnectionSource),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConnectionSource {
    pub name: String,
    /// 資料庫來源：整表讀取（與 query 擇一）
    pub table: Option<String>,
    /// 資料庫來源：自訂查詢（與 table 擇一）
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileSource {
    pub path: String,
    pub sheet: Option<String>,
    pub encoding: Option<String>,
    pub has_header: Option<bool>,
}

/// 腳本標頭：讓 .etl 檔自包含來源與目標。
/// 安全政策：TARGET 僅支援 CONNECTION('已儲存連線名稱') —
/// 密碼一律留在 OS keychain，絕不寫入腳本檔。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScriptHeader {
    pub source: Option<SourceDecl>,
    pub target_connection: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub header: ScriptHeader,
    pub statements: Vec<Statement>,
}

/// 解析 / 驗證錯誤（含行號，供編輯器標示）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptIssue {
    pub line: usize,
    pub message: String,
}
