//! ETL 腳本 DSL v0.2 的抽象語法樹（子句化）。
//!
//! 工作主體為一組有序子句：
//! ```text
//! FROM → JOIN* → WHERE? → INTO → (ON → MATCHED?/NOT MATCHED?)? → ADD/UPDATE
//! ```
//! 範例（查表 + 新增）：
//! ```text
//! WORK 'EluCloudAccount綁定EnterId' {
//!   FROM entra   = SOURCE.[users]
//!   JOIN account = TARGET.[dbo].[DirectoryAccounts]
//!     ON (entra.[userPrincipalName] == account.[Email])
//!   INTO TARGET.[dbo].[ExternalIdentityMappings]
//!   ADD {
//!      [Id]                 = Gen.ULID
//!     ,[AccountId]          = account.[Id]
//!     ,[ExternalId]         = entra.[id]
//!     ,[ExternalSystemType] = N'MICROSOFT_ENTRA_ID'
//!     ,[Label]              = N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'
//!   }
//! }
//! ```
//! 兩種「對應」分開：`JOIN … ON` 是 join key（查表，少一筆＝查無對應）；
//! 頂層 `ON` 是 merge key（去重，少一筆＝目標尚未存在 → 該寫入）。
//!
//! 舊式 `If … 換行 [表] ADD {…}` 與裸 `[表] ADD {…}` 仍相容解析，
//! parser 會正規化為本檔的新 AST（欄位一律 alias 限定）。

/// 欄位參照：`alias.[column]`；alias 對應某個 FROM / JOIN / INTO 綁定。
#[derive(Debug, Clone, PartialEq)]
pub struct ColRef {
    pub alias: String,
    pub column: String,
    pub line: usize,
}

impl ColRef {
    /// 正規化別名（小寫），供綁定歸屬比對。
    pub fn alias_key(&self) -> String {
        self.alias.to_lowercase()
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

/// 指派值 / 條件運算元：欄位參照、字面值、產生器，或 `{…}` 合成欄位。
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Col(ColRef),
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    Gen(GenKind),
    /// 合成欄位（字串模板）：各項轉為文字後串接（NULL 視為空字串）。
    /// 來源語法為帶 `{…}` 插值的字串，動態值寫在大括號內，如
    /// `N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'`
    /// （舊式 `'前綴' + [欄位]` 串接仍可解析，會正規化為此模板形式）。
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
            // 欄位一律 alias 限定：account.[Id]
            Expr::Col(r) => format!("{}.[{}]", r.alias, r.column),
            Expr::Text(s) => format!("N'{}'", s.replace('\'', "''")),
            Expr::Int(v) => v.to_string(),
            Expr::Float(v) => v.to_string(),
            Expr::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
            Expr::Null => "NULL".to_string(),
            Expr::Gen(k) => format!("Gen.{}", k.label()),
            // 字串模板：固定文字直接寫入引號內，動態值（欄位 / Gen / 數值）放在 {…} 內；
            // 字面大括號以 {{ }} 跳脫。如 N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'
            Expr::Concat(parts) => {
                let mut s = String::from("N'");
                for p in parts {
                    match p {
                        Expr::Text(t) => {
                            for ch in t.chars() {
                                match ch {
                                    '\'' => s.push_str("''"),
                                    '{' => s.push_str("{{"),
                                    '}' => s.push_str("}}"),
                                    c => s.push(c),
                                }
                            }
                        }
                        other => {
                            s.push('{');
                            s.push_str(&other.to_dsl());
                            s.push('}');
                        }
                    }
                }
                s.push('\'');
                s
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub target_column: String,
    pub value: Expr,
    pub line: usize,
}

/// 來源連線參照：FROM / JOIN / INTO 的 `SOURCE` / `TARGET`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnRef {
    Source,
    Target,
}

impl ConnRef {
    pub fn label(&self) -> &'static str {
        match self {
            ConnRef::Source => "SOURCE",
            ConnRef::Target => "TARGET",
        }
    }
}

/// 別名綁定：`<alias> = <conn>.[…]`（FROM / JOIN）。
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub alias: String,
    pub conn: ConnRef,
    /// 資料表路徑各段（檔案來源可為空，作名目用途）
    pub table: Vec<String>,
    pub line: usize,
}

/// 查表未命中政策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinPolicy {
    /// 未命中 → 該列進錯誤報告（預設，＝現況「啟用比對」行為）
    Inner,
    /// 未命中 → 該查表欄位取 NULL 照常寫（選用）
    Left,
}

/// 查表 / Lookup join：`JOIN <alias> = <conn>.[…] ON (<條件>)`。
#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub binding: Binding,
    pub on: Condition,
    pub policy: JoinPolicy,
}

/// 寫入目標：`INTO [<alias> =] <conn>.[…]`（要用合併鍵時取別名供 `ON` 引用）。
/// 命名避開 std `Into` trait。
#[derive(Debug, Clone, PartialEq)]
pub struct IntoClause {
    pub conn: ConnRef,
    pub table: Vec<String>,
    pub alias: Option<String>,
    pub line: usize,
}

/// 比較運算子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

impl CmpOp {
    pub fn symbol(&self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Gt => ">",
            CmpOp::Lt => "<",
            CmpOp::Ge => ">=",
            CmpOp::Le => "<=",
        }
    }
}

/// 單一比較式：`<左> <op> <右>`。
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub left: Expr,
    pub op: CmpOp,
    pub right: Expr,
    pub line: usize,
}

/// 空值 / 空字串判斷：`<運算式> IS [NOT] EMPTY`。
#[derive(Debug, Clone, PartialEq)]
pub struct IsEmptyCheck {
    pub expr: Expr,
    /// true = `IS NOT EMPTY`
    pub negated: bool,
    pub line: usize,
}

/// 條件（JOIN ON / merge ON / WHERE 共用）：布林樹 + 比較 / IS [NOT] EMPTY / IN / LIKE / BETWEEN。
/// 優先級：`||` < `&&` < `!` < 葉節點（解析時建樹）。
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    And(Vec<Condition>),
    Or(Vec<Condition>),
    Not(Box<Condition>),
    Compare(Comparison),
    IsEmpty(IsEmptyCheck),
    /// `<expr> [NOT] IN (<list>)`
    In {
        expr: Expr,
        list: Vec<Expr>,
        negated: bool,
        line: usize,
    },
    /// `<expr> [NOT] LIKE <pattern>`（`%` 多字元、`_` 單字元，不分大小寫）
    Like {
        expr: Expr,
        pattern: Expr,
        negated: bool,
        line: usize,
    },
    /// `<expr> [NOT] BETWEEN <low> AND <high>`（含端點）
    Between {
        expr: Expr,
        low: Expr,
        high: Expr,
        negated: bool,
        line: usize,
    },
}

impl Condition {
    fn is_leaf(&self) -> bool {
        matches!(
            self,
            Condition::Compare(_)
                | Condition::IsEmpty(_)
                | Condition::In { .. }
                | Condition::Like { .. }
                | Condition::Between { .. }
        )
    }

    /// 走訪條件內所有欄位參照（遞迴）。
    pub fn col_refs(&self) -> Vec<&ColRef> {
        let mut out: Vec<&ColRef> = Vec::new();
        match self {
            Condition::And(v) | Condition::Or(v) => {
                for c in v {
                    out.extend(c.col_refs());
                }
            }
            Condition::Not(c) => out.extend(c.col_refs()),
            Condition::Compare(c) => {
                out.extend(c.left.col_refs());
                out.extend(c.right.col_refs());
            }
            Condition::IsEmpty(e) => out.extend(e.expr.col_refs()),
            Condition::In { expr, list, .. } => {
                out.extend(expr.col_refs());
                for e in list {
                    out.extend(e.col_refs());
                }
            }
            Condition::Like { expr, pattern, .. } => {
                out.extend(expr.col_refs());
                out.extend(pattern.col_refs());
            }
            Condition::Between {
                expr, low, high, ..
            } => {
                out.extend(expr.col_refs());
                out.extend(low.col_refs());
                out.extend(high.col_refs());
            }
        }
        out
    }

    /// 若為「扁平 AND 的等值比較（或單一等值比較）」回傳等式清單，否則 None。
    /// JOIN ON / merge ON 要求等值合取，藉此抽取 hash key。
    pub fn as_eq_conjunction(&self) -> Option<Vec<&Comparison>> {
        fn eq(c: &Condition) -> Option<&Comparison> {
            match c {
                Condition::Compare(cmp) if cmp.op == CmpOp::Eq => Some(cmp),
                _ => None,
            }
        }
        match self {
            Condition::And(v) => v.iter().map(eq).collect(),
            other => eq(other).map(|c| vec![c]),
        }
    }

    /// 若為「扁平 AND 的葉節點（或單一葉節點）」回傳葉清單，否則 None（GUI 可視化判斷）。
    pub fn flat_rows(&self) -> Option<Vec<&Condition>> {
        match self {
            Condition::And(v) if v.iter().all(Condition::is_leaf) => Some(v.iter().collect()),
            other if other.is_leaf() => Some(vec![other]),
            _ => None,
        }
    }

    /// 正規 DSL 文字（raw 條件 round-trip）。優先級：Or=1 < And=2 < 葉/Not=3。
    pub fn to_dsl(&self) -> String {
        self.fmt_prec(0)
    }

    fn fmt_prec(&self, parent: u8) -> String {
        let (prec, s) = match self {
            Condition::Or(v) => (
                1,
                v.iter().map(|c| c.fmt_prec(1)).collect::<Vec<_>>().join(" || "),
            ),
            Condition::And(v) => (
                2,
                v.iter().map(|c| c.fmt_prec(2)).collect::<Vec<_>>().join(" && "),
            ),
            Condition::Not(c) => (3, format!("!{}", c.fmt_prec(3))),
            Condition::Compare(c) => (
                3,
                format!("{} {} {}", c.left.to_dsl(), c.op.symbol(), c.right.to_dsl()),
            ),
            Condition::IsEmpty(e) => (
                3,
                format!(
                    "{} IS {}EMPTY",
                    e.expr.to_dsl(),
                    if e.negated { "NOT " } else { "" }
                ),
            ),
            Condition::In {
                expr,
                list,
                negated,
                ..
            } => (
                3,
                format!(
                    "{} {}IN ({})",
                    expr.to_dsl(),
                    if *negated { "NOT " } else { "" },
                    list.iter().map(Expr::to_dsl).collect::<Vec<_>>().join(", ")
                ),
            ),
            Condition::Like {
                expr,
                pattern,
                negated,
                ..
            } => (
                3,
                format!(
                    "{} {}LIKE {}",
                    expr.to_dsl(),
                    if *negated { "NOT " } else { "" },
                    pattern.to_dsl()
                ),
            ),
            Condition::Between {
                expr,
                low,
                high,
                negated,
                ..
            } => (
                3,
                format!(
                    "{} {}BETWEEN {} AND {}",
                    expr.to_dsl(),
                    if *negated { "NOT " } else { "" },
                    low.to_dsl(),
                    high.to_dsl()
                ),
            ),
        };
        if prec < parent {
            format!("({s})")
        } else {
            s
        }
    }
}

/// 寫入動作。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Add(Vec<Assignment>),
    Update(Vec<Assignment>),
    Skip,
    Delete,
}

impl Action {
    /// 動作關鍵字（序列化 / 顯示用）。
    pub fn keyword(&self) -> &'static str {
        match self {
            Action::Add(_) => "ADD",
            Action::Update(_) => "UPDATE",
            Action::Skip => "SKIP",
            Action::Delete => "DELETE",
        }
    }

    pub fn assignments(&self) -> &[Assignment] {
        match self {
            Action::Add(a) | Action::Update(a) => a,
            Action::Skip | Action::Delete => &[],
        }
    }
}

/// 合併（MERGE / upsert）：頂層 `ON (<條件>)` + 分支。
#[derive(Debug, Clone, PartialEq)]
pub struct Merge {
    pub on: Condition,
    pub matched: Option<Action>,
    pub not_matched: Option<Action>,
}

/// 一個轉換工作單元（`WORK '名稱' { … }`，或舊式相容陳述式正規化而來）。
#[derive(Debug, Clone, PartialEq)]
pub struct Work {
    pub name: Option<String>,
    /// 主來源：逐列迭代的對象（決定 body 跑幾次）
    pub from: Binding,
    /// 查表（0..N）
    pub joins: Vec<Join>,
    /// 過濾
    pub where_: Option<Condition>,
    /// 寫入目標
    pub into: IntoClause,
    /// 合併語意（None → `action` 為純附加）
    pub merge: Option<Merge>,
    /// 無 merge 時的頂層動作（通常為 ADD）
    pub action: Option<Action>,
    pub line: usize,
}

impl Work {
    /// 此工作所有別名綁定（FROM + JOIN；INTO 別名另計）。供正規化 / 執行歸屬。
    pub fn source_bindings(&self) -> Vec<&Binding> {
        let mut v = vec![&self.from];
        v.extend(self.joins.iter().map(|j| &j.binding));
        v
    }

    /// 此工作所有寫入動作（頂層 + merge 分支），供欄位走訪。
    pub fn actions(&self) -> Vec<&Action> {
        let mut v: Vec<&Action> = Vec::new();
        if let Some(a) = &self.action {
            v.push(a);
        }
        if let Some(m) = &self.merge {
            v.extend(m.matched.iter());
            v.extend(m.not_matched.iter());
        }
        v
    }
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
    pub works: Vec<Work>,
}

/// 解析 / 驗證錯誤（含行號，供編輯器標示）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptIssue {
    pub line: usize,
    pub message: String,
}
