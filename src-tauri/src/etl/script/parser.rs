//! ETL 腳本 DSL v0.2 的手寫 lexer + recursive descent parser。
//!
//! 子句序：`FROM → JOIN* → WHERE? → INTO → (ON → MATCHED?/NOT MATCHED?)? → ADD/UPDATE`。
//! 關鍵字不分大小寫；`--` 或 `//` 為單行註解，`///` ~ `///` 為多行註解；
//! 識別字可用 `[名稱]` 或裸字。欄位一律寫成 `別名.[欄位]`。
//!
//! 舊式 `If <條件> 換行 [表] ADD {…}` 與裸 `[表] ADD {…}` 仍相容解析，
//! 解析後正規化為新 AST（合成 FROM / JOIN / INTO 與具名別名），讓視覺編輯器只面對新模型。

use crate::etl::script::ast::{
    Action, Assignment, Binding, ColRef, CmpOp, Comparison, Condition, ConnRef, ConnectionSource,
    Expr, FileSource, GenKind, IntoClause, IsEmptyCheck, Join, JoinPolicy, Merge, Script,
    ScriptHeader, ScriptIssue, SourceDecl, Work,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kw {
    Work,
    From,
    Join,
    Where,
    Into,
    On,
    Matched,
    Not,
    Add,
    Update,
    Skip,
    Delete,
    Go,
    Is,
    Empty,
    In,
    Like,
    Between,
    Inner,
    Left,
    Source,
    Target,
    Null,
    True,
    False,
    If,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Number(String),
    Str(String),
    Dot,
    Comma,
    LBrace,
    RBrace,
    LParen,
    RParen,
    Plus,
    Assign, // =
    Eq,     // ==
    Ne,     // !=
    Gt,     // >
    Lt,     // <
    Ge,     // >=
    Le,     // <=
    AndAnd, // &&
    OrOr,   // ||
    Bang,   // !
    Kw(Kw),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: usize,
}

fn err(line: usize, message: impl Into<String>) -> ScriptIssue {
    ScriptIssue {
        line,
        message: message.into(),
    }
}

// ---------- Lexer ----------

fn lex(input: &str) -> Result<Vec<Token>, ScriptIssue> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut line = 1;

    macro_rules! push {
        ($t:expr, $n:expr) => {{
            tokens.push(Token { tok: $t, line });
            i += $n;
        }};
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'/') && chars.get(i + 2) == Some(&'/') => {
                // 多行註解 /// … ///（起止皆為 ///，內部可跨行）
                let start = line;
                i += 3;
                loop {
                    match chars.get(i) {
                        None => return Err(err(start, "未閉合的多行註解（缺少結尾 ///）")),
                        Some('/')
                            if chars.get(i + 1) == Some(&'/')
                                && chars.get(i + 2) == Some(&'/') =>
                        {
                            i += 3;
                            break;
                        }
                        Some('\n') => {
                            line += 1;
                            i += 1;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '[' => {
                let start = line;
                let mut name = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        Some(']') => {
                            i += 1;
                            break;
                        }
                        Some('\n') | None => {
                            return Err(err(start, "未閉合的 [ 識別字（缺少 ]）"));
                        }
                        Some(&ch) => {
                            name.push(ch);
                            i += 1;
                        }
                    }
                }
                if name.trim().is_empty() {
                    return Err(err(start, "[] 內不可為空"));
                }
                tokens.push(Token {
                    tok: Tok::Ident(name.trim().to_string()),
                    line: start,
                });
            }
            '\'' => {
                let (s, ni) = lex_string(&chars, i + 1, line)?;
                tokens.push(Token {
                    tok: Tok::Str(s),
                    line,
                });
                i = ni;
            }
            'N' | 'n' if chars.get(i + 1) == Some(&'\'') => {
                let (s, ni) = lex_string(&chars, i + 2, line)?;
                tokens.push(Token {
                    tok: Tok::Str(s),
                    line,
                });
                i = ni;
            }
            '=' if chars.get(i + 1) == Some(&'=') => push!(Tok::Eq, 2),
            '=' => push!(Tok::Assign, 1),
            '!' if chars.get(i + 1) == Some(&'=') => push!(Tok::Ne, 2),
            '!' => push!(Tok::Bang, 1),
            '>' if chars.get(i + 1) == Some(&'=') => push!(Tok::Ge, 2),
            '>' => push!(Tok::Gt, 1),
            '<' if chars.get(i + 1) == Some(&'=') => push!(Tok::Le, 2),
            '<' => push!(Tok::Lt, 1),
            '&' if chars.get(i + 1) == Some(&'&') => push!(Tok::AndAnd, 2),
            '|' if chars.get(i + 1) == Some(&'|') => push!(Tok::OrOr, 2),
            '.' => push!(Tok::Dot, 1),
            ',' => push!(Tok::Comma, 1),
            '{' => push!(Tok::LBrace, 1),
            '}' => push!(Tok::RBrace, 1),
            '(' => push!(Tok::LParen, 1),
            ')' => push!(Tok::RParen, 1),
            '+' => push!(Tok::Plus, 1),
            c if c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())) =>
            {
                let mut num = String::from(c);
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    num.push(chars[i]);
                    i += 1;
                }
                tokens.push(Token {
                    tok: Tok::Number(num),
                    line,
                });
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut word = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    word.push(chars[i]);
                    i += 1;
                }
                let tok = match word.to_uppercase().as_str() {
                    "WORK" => Tok::Kw(Kw::Work),
                    "FROM" => Tok::Kw(Kw::From),
                    "JOIN" => Tok::Kw(Kw::Join),
                    "WHERE" => Tok::Kw(Kw::Where),
                    "INTO" => Tok::Kw(Kw::Into),
                    "ON" => Tok::Kw(Kw::On),
                    "MATCHED" => Tok::Kw(Kw::Matched),
                    "NOT" => Tok::Kw(Kw::Not),
                    "ADD" => Tok::Kw(Kw::Add),
                    "UPDATE" => Tok::Kw(Kw::Update),
                    "SKIP" => Tok::Kw(Kw::Skip),
                    "DELETE" => Tok::Kw(Kw::Delete),
                    "GO" => Tok::Kw(Kw::Go),
                    "IS" => Tok::Kw(Kw::Is),
                    "EMPTY" => Tok::Kw(Kw::Empty),
                    "IN" => Tok::Kw(Kw::In),
                    "LIKE" => Tok::Kw(Kw::Like),
                    "BETWEEN" => Tok::Kw(Kw::Between),
                    "INNER" => Tok::Kw(Kw::Inner),
                    "LEFT" => Tok::Kw(Kw::Left),
                    "SOURCE" => Tok::Kw(Kw::Source),
                    "TARGET" => Tok::Kw(Kw::Target),
                    "NULL" => Tok::Kw(Kw::Null),
                    "TRUE" => Tok::Kw(Kw::True),
                    "FALSE" => Tok::Kw(Kw::False),
                    "IF" => Tok::Kw(Kw::If),
                    _ => Tok::Ident(word),
                };
                tokens.push(Token { tok, line });
            }
            other => {
                return Err(err(line, format!("無法辨識的字元: {other:?}")));
            }
        }
    }
    Ok(tokens)
}

/// 讀取字串字面值（起點在開頭引號之後）；`''` 為跳脫的單引號。
fn lex_string(chars: &[char], mut i: usize, line: usize) -> Result<(String, usize), ScriptIssue> {
    let mut s = String::new();
    loop {
        match chars.get(i) {
            Some('\'') if chars.get(i + 1) == Some(&'\'') => {
                s.push('\'');
                i += 2;
            }
            Some('\'') => return Ok((s, i + 1)),
            Some('\n') | None => return Err(err(line, "未閉合的字串（缺少 '）")),
            Some(&ch) => {
                s.push(ch);
                i += 1;
            }
        }
    }
}

// ---------- 別名作用域（欄位參照正規化） ----------

#[derive(Clone)]
struct BindingInfo {
    alias: String,
    alias_lower: String,
    table_lower: Vec<String>,
}

/// 解析期的別名作用域：把欄位的 `parts`（`別名.[欄位]` 或舊式 `[db].[表].[欄位]`）正規化成 `ColRef{alias,column}`。
#[derive(Clone, Default)]
struct Scope {
    from_alias: String,
    bindings: Vec<BindingInfo>,
    /// 舊式相容模式：以表路徑比對別名（新語法則嚴格要求 `別名.[欄位]`）
    legacy: bool,
}

impl Scope {
    fn register(&mut self, alias: &str, table: &[String]) {
        self.bindings.push(BindingInfo {
            alias: alias.to_string(),
            alias_lower: alias.to_lowercase(),
            table_lower: table.iter().map(|s| s.to_lowercase()).collect(),
        });
    }

    fn canonical(&self, lower: &str) -> Option<String> {
        self.bindings
            .iter()
            .find(|b| b.alias_lower == lower)
            .map(|b| b.alias.clone())
    }

    fn resolve(&self, parts: &[String], line: usize) -> Result<ColRef, ScriptIssue> {
        if parts.is_empty() {
            return Err(err(line, "空的欄位參照"));
        }
        if !self.legacy {
            // 新語法：別名.[欄位]（1 段時隱含為主來源）
            if parts.len() == 1 {
                return Ok(ColRef {
                    alias: self.from_alias.clone(),
                    column: parts[0].clone(),
                    line,
                });
            }
            if parts.len() == 2 {
                let head = parts[0].to_lowercase();
                return self
                    .canonical(&head)
                    .map(|alias| ColRef {
                        alias,
                        column: parts[1].clone(),
                        line,
                    })
                    .ok_or_else(|| {
                        err(
                            line,
                            format!(
                                "未知別名 [{}]——欄位需寫成 別名.[欄位]，別名須由 FROM / JOIN / INTO 宣告",
                                parts[0]
                            ),
                        )
                    });
            }
            return Err(err(
                line,
                format!("欄位參照應為 別名.[欄位]，但收到 {}", parts.join(".")),
            ));
        }
        // 舊式：表路徑限定 → 正規化為別名
        let column = parts.last().unwrap().clone();
        let table: Vec<String> = parts[..parts.len() - 1]
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        if table.is_empty() || table == ["source"] {
            return Ok(ColRef {
                alias: self.from_alias.clone(),
                column,
                line,
            });
        }
        if let Some(b) = self.bindings.iter().find(|b| b.table_lower == table) {
            return Ok(ColRef {
                alias: b.alias.clone(),
                column,
                line,
            });
        }
        // 無法比對（無 join 或裸表）→ 視為來源
        Ok(ColRef {
            alias: self.from_alias.clone(),
            column,
            line,
        })
    }
}

/// 由表路徑推導唯一別名（取末段小寫、去非英數字；撞名加序號）。
fn derive_alias(table: &[String], fallback: &str, taken: &[String]) -> String {
    let base: String = table
        .last()
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let taken_lower: Vec<String> = taken.iter().map(|t| t.to_lowercase()).collect();
    if !taken_lower.contains(&base) {
        return base;
    }
    for n in 2.. {
        let cand = format!("{base}{n}");
        if !taken_lower.contains(&cand.to_lowercase()) {
            return cand;
        }
    }
    unreachable!()
}

// ---------- Parser ----------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    scope: Scope,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            scope: Scope::default(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.tokens.get(self.pos + n).map(|t| &t.tok)
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.pos.min(self.tokens.len().saturating_sub(1)))
            .map_or(0, |t| t.line)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat(&mut self, tok: &Tok, what: &str) -> Result<(), ScriptIssue> {
        let line = self.line();
        if self.peek() == Some(tok) {
            self.next();
            Ok(())
        } else {
            Err(err(line, format!("預期 {what}")))
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<(String, usize), ScriptIssue> {
        let line = self.line();
        match self.next() {
            Some(Token {
                tok: Tok::Ident(s),
                line,
            }) => Ok((s, line)),
            _ => Err(err(line, format!("預期 {what}"))),
        }
    }

    fn expect_str(&mut self, line: usize, what: &str) -> Result<String, ScriptIssue> {
        match self.next() {
            Some(Token {
                tok: Tok::Str(s), ..
            }) => Ok(s),
            _ => Err(err(line, what.to_string())),
        }
    }

    /// 讀取 `a.b.c` 的各段（識別字，含 `[名稱]`）；至少 1 段。回傳原樣（未正規化）。
    fn parse_raw_parts(&mut self) -> Result<(Vec<String>, usize), ScriptIssue> {
        let (first, line) = self.expect_ident("識別字")?;
        let mut parts = vec![first];
        while self.peek() == Some(&Tok::Dot) {
            self.next();
            let (p, _) = self.expect_ident("`.` 之後的識別字")?;
            parts.push(p);
        }
        Ok((parts, line))
    }

    // ---- 運算式 ----

    /// 運算式：單一項，或舊式以 `+` 串接的合成欄位（仍相容；新語法為字串模板）。
    fn parse_expr(&mut self) -> Result<Expr, ScriptIssue> {
        let first = self.parse_term()?;
        if self.peek() != Some(&Tok::Plus) {
            return Ok(first);
        }
        let mut parts: Vec<Expr> = Vec::new();
        push_concat_part(&mut parts, first);
        while self.peek() == Some(&Tok::Plus) {
            self.next();
            let term = self.parse_term()?;
            push_concat_part(&mut parts, term);
        }
        Ok(Expr::Concat(parts))
    }

    fn parse_term(&mut self) -> Result<Expr, ScriptIssue> {
        let line = self.line();
        match self.peek() {
            Some(Tok::Str(_)) => {
                let Some(Token {
                    tok: Tok::Str(s),
                    line,
                }) = self.next()
                else {
                    unreachable!()
                };
                parse_template(&s, line, &self.scope)
            }
            Some(Tok::Number(_)) => {
                let Some(Token {
                    tok: Tok::Number(n),
                    ..
                }) = self.next()
                else {
                    unreachable!()
                };
                if n.contains('.') {
                    n.parse::<f64>()
                        .map(Expr::Float)
                        .map_err(|_| err(line, format!("無效的數字: {n}")))
                } else {
                    n.parse::<i64>()
                        .map(Expr::Int)
                        .map_err(|_| err(line, format!("無效的數字: {n}")))
                }
            }
            Some(Tok::Kw(Kw::Null)) => {
                self.next();
                Ok(Expr::Null)
            }
            Some(Tok::Kw(Kw::True)) => {
                self.next();
                Ok(Expr::Bool(true))
            }
            Some(Tok::Kw(Kw::False)) => {
                self.next();
                Ok(Expr::Bool(false))
            }
            Some(Tok::Ident(_)) => {
                let (parts, line) = self.parse_raw_parts()?;
                if parts.len() >= 2 && parts[0].eq_ignore_ascii_case("gen") {
                    return self.finish_gen(&parts, line);
                }
                Ok(Expr::Col(self.scope.resolve(&parts, line)?))
            }
            _ => Err(err(line, "預期欄位參照或字面值（'文字'、數字、NULL）")),
        }
    }

    /// `Gen.XXX` / `Gen.XXX(Text)` 產生器（Gen 為保留前綴）。
    fn finish_gen(&mut self, parts: &[String], line: usize) -> Result<Expr, ScriptIssue> {
        if parts.len() != 2 {
            return Err(err(line, "產生器格式為 Gen.XXX 或 Gen.XXX(Text)"));
        }
        let name = &parts[1];
        let mut text_variant = false;
        if self.peek() == Some(&Tok::LParen) {
            self.next();
            match self.next() {
                Some(Token {
                    tok: Tok::Ident(s), ..
                }) if s.eq_ignore_ascii_case("text") => {}
                _ => return Err(err(line, "產生器參數僅支援 (Text)")),
            }
            if self.next().map(|t| t.tok) != Some(Tok::RParen) {
                return Err(err(line, "產生器缺少 `)`"));
            }
            text_variant = true;
        }
        GenKind::parse(name, text_variant)
            .map(Expr::Gen)
            .ok_or_else(|| {
                err(
                    line,
                    format!(
                        "未知的產生器 Gen.{}{}（支援：{}）",
                        name,
                        if text_variant { "(Text)" } else { "" },
                        GenKind::ALL_LABELS
                    ),
                )
            })
    }

    // ---- 條件（布林樹：`||` < `&&` < `!` < 葉節點） ----

    fn parse_paren_condition(&mut self) -> Result<Condition, ScriptIssue> {
        self.eat(&Tok::LParen, "`(`")?;
        let cond = self.parse_or()?;
        self.eat(&Tok::RParen, "`)`")?;
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Condition, ScriptIssue> {
        let mut terms = vec![self.parse_and()?];
        while self.peek() == Some(&Tok::OrOr) {
            self.next();
            terms.push(self.parse_and()?);
        }
        Ok(if terms.len() == 1 {
            terms.pop().unwrap()
        } else {
            Condition::Or(terms)
        })
    }

    fn parse_and(&mut self) -> Result<Condition, ScriptIssue> {
        let mut terms = vec![self.parse_not()?];
        while self.peek() == Some(&Tok::AndAnd) {
            self.next();
            terms.push(self.parse_not()?);
        }
        Ok(if terms.len() == 1 {
            terms.pop().unwrap()
        } else {
            Condition::And(terms)
        })
    }

    fn parse_not(&mut self) -> Result<Condition, ScriptIssue> {
        if self.peek() == Some(&Tok::Bang) {
            self.next();
            return Ok(Condition::Not(Box::new(self.parse_not()?)));
        }
        self.parse_primary()
    }

    /// 葉節點：群組 `( … )` / 比較 / `IS [NOT] EMPTY` / `[NOT] IN/LIKE/BETWEEN`。
    fn parse_primary(&mut self) -> Result<Condition, ScriptIssue> {
        if self.peek() == Some(&Tok::LParen) {
            self.next();
            let cond = self.parse_or()?;
            self.eat(&Tok::RParen, "群組條件結尾 `)`")?;
            return Ok(cond);
        }
        let line = self.line();
        let left = self.parse_expr()?;

        // IS [NOT] EMPTY
        if self.peek() == Some(&Tok::Kw(Kw::Is)) {
            self.next();
            let negated = if self.peek() == Some(&Tok::Kw(Kw::Not)) {
                self.next();
                true
            } else {
                false
            };
            self.eat(&Tok::Kw(Kw::Empty), "EMPTY")?;
            return Ok(Condition::IsEmpty(IsEmptyCheck {
                expr: left,
                negated,
                line,
            }));
        }

        // [NOT] IN / LIKE / BETWEEN
        let negated = if self.peek() == Some(&Tok::Kw(Kw::Not)) {
            self.next();
            true
        } else {
            false
        };
        match self.peek() {
            Some(Tok::Kw(Kw::In)) => {
                self.next();
                self.eat(&Tok::LParen, "IN 之後的 `(`")?;
                let mut list = vec![self.parse_expr()?];
                while self.peek() == Some(&Tok::Comma) {
                    self.next();
                    list.push(self.parse_expr()?);
                }
                self.eat(&Tok::RParen, "IN 清單結尾 `)`")?;
                Ok(Condition::In {
                    expr: left,
                    list,
                    negated,
                    line,
                })
            }
            Some(Tok::Kw(Kw::Like)) => {
                self.next();
                let pattern = self.parse_expr()?;
                Ok(Condition::Like {
                    expr: left,
                    pattern,
                    negated,
                    line,
                })
            }
            Some(Tok::Kw(Kw::Between)) => {
                self.next();
                let low = self.parse_expr()?;
                // BETWEEN low AND high：此 AND 為語法字（裸識別字），非邏輯 `&&`
                match self.next() {
                    Some(Token {
                        tok: Tok::Ident(s),
                        ..
                    }) if s.eq_ignore_ascii_case("AND") => {}
                    _ => {
                        return Err(err(
                            line,
                            "BETWEEN 需以 AND 分隔上下界（如 BETWEEN 1 AND 10）",
                        ))
                    }
                }
                let high = self.parse_expr()?;
                Ok(Condition::Between {
                    expr: left,
                    low,
                    high,
                    negated,
                    line,
                })
            }
            _ => {
                if negated {
                    return Err(err(line, "NOT 之後預期 IN / LIKE / BETWEEN"));
                }
                let op = match self.next().map(|t| t.tok) {
                    Some(Tok::Eq) => CmpOp::Eq,
                    Some(Tok::Ne) => CmpOp::Ne,
                    Some(Tok::Gt) => CmpOp::Gt,
                    Some(Tok::Lt) => CmpOp::Lt,
                    Some(Tok::Ge) => CmpOp::Ge,
                    Some(Tok::Le) => CmpOp::Le,
                    _ => {
                        return Err(err(
                            line,
                            "預期比較運算子（== != > < >= <=）、IS [NOT] EMPTY 或 [NOT] IN/LIKE/BETWEEN",
                        ))
                    }
                };
                let right = self.parse_expr()?;
                Ok(Condition::Compare(Comparison {
                    left,
                    op,
                    right,
                    line,
                }))
            }
        }
    }

    // ---- 綁定 / 子句 ----

    fn parse_conn_ref(&mut self, line: usize) -> Result<ConnRef, ScriptIssue> {
        match self.next().map(|t| t.tok) {
            Some(Tok::Kw(Kw::Source)) => Ok(ConnRef::Source),
            Some(Tok::Kw(Kw::Target)) => Ok(ConnRef::Target),
            _ => Err(err(line, "FROM / JOIN / INTO 來源需為 SOURCE 或 TARGET")),
        }
    }

    /// `<alias> = <conn>.[seg].[seg]…`（表路徑可為空，如 `FROM rows = SOURCE`）。
    fn parse_binding(&mut self) -> Result<Binding, ScriptIssue> {
        let (alias, line) = self.expect_ident("別名（如 entra / account）")?;
        self.eat(&Tok::Assign, format!("別名 {alias} 之後的 `=`").as_str())?;
        let conn = self.parse_conn_ref(line)?;
        let table = self.parse_table_path()?;
        Ok(Binding {
            alias,
            conn,
            table,
            line,
        })
    }

    /// `.[seg].[seg]…`（0..N 段）。
    fn parse_table_path(&mut self) -> Result<Vec<String>, ScriptIssue> {
        let mut table = Vec::new();
        while self.peek() == Some(&Tok::Dot) {
            self.next();
            let (s, _) = self.expect_ident("`.` 之後的資料表段")?;
            table.push(s);
        }
        Ok(table)
    }

    fn parse_into(&mut self) -> Result<IntoClause, ScriptIssue> {
        let line = self.line();
        self.eat(&Tok::Kw(Kw::Into), "INTO")?;
        // 可選別名前綴：`<alias> =`
        let alias = if matches!(self.peek(), Some(Tok::Ident(_)))
            && self.peek_at(1) == Some(&Tok::Assign)
        {
            let (a, _) = self.expect_ident("INTO 別名")?;
            self.next(); // =
            Some(a)
        } else {
            None
        };
        let conn = self.parse_conn_ref(line)?;
        if conn != ConnRef::Target {
            return Err(err(line, "INTO 目標必須是 TARGET（檔案不可當目標）"));
        }
        let table = self.parse_table_path()?;
        if table.is_empty() {
            return Err(err(line, "INTO 之後需指定目標資料表"));
        }
        // 舊式 `INTO … AS x` 不接受
        if matches!(self.peek(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("AS")) {
            return Err(err(
                self.line(),
                "別名請寫成 `INTO 別名 = TARGET.[…]`（不支援 `AS`）",
            ));
        }
        Ok(IntoClause {
            conn,
            table,
            alias,
            line,
        })
    }

    /// `{ 欄位 = 值, ... }` 指派區塊。
    fn parse_assignment_block(&mut self) -> Result<Vec<Assignment>, ScriptIssue> {
        self.eat(&Tok::LBrace, "`{`")?;
        let mut assignments = Vec::new();
        loop {
            if self.peek() == Some(&Tok::RBrace) {
                self.next();
                break;
            }
            if !assignments.is_empty() && self.next().map(|t| t.tok) != Some(Tok::Comma) {
                return Err(err(self.line(), "欄位指派之間需以 `,` 分隔"));
            }
            let (target_column, line) = self.expect_ident("目標欄位名稱")?;
            self.eat(&Tok::Assign, format!("欄位 {target_column} 之後的 `=`").as_str())?;
            let value = self.parse_expr()?;
            assignments.push(Assignment {
                target_column,
                value,
                line,
            });
        }
        Ok(assignments)
    }

    /// `ADD/UPDATE { … }` 或 `SKIP` / `DELETE`。
    fn parse_action(&mut self) -> Result<Action, ScriptIssue> {
        let line = self.line();
        match self.next().map(|t| t.tok) {
            Some(Tok::Kw(Kw::Add)) => Ok(Action::Add(self.parse_assignment_block()?)),
            Some(Tok::Kw(Kw::Update)) => Ok(Action::Update(self.parse_assignment_block()?)),
            Some(Tok::Kw(Kw::Skip)) => Ok(Action::Skip),
            Some(Tok::Kw(Kw::Delete)) => Ok(Action::Delete),
            _ => Err(err(line, "預期動作 ADD / UPDATE / SKIP / DELETE")),
        }
    }

    /// `MATCHED { <動作> }` / `NOT MATCHED { <動作> }` 的分支主體。
    fn parse_merge_branch(&mut self) -> Result<Action, ScriptIssue> {
        self.eat(&Tok::LBrace, "MATCHED / NOT MATCHED 之後的 `{`")?;
        let action = self.parse_action()?;
        self.eat(&Tok::RBrace, "MATCHED / NOT MATCHED 區塊結尾 `}`")?;
        Ok(action)
    }

    // ---- 工作主體（新語法 / 舊式相容） ----

    fn parse_work_body(&mut self, name: Option<String>, line: usize) -> Result<Work, ScriptIssue> {
        match self.peek() {
            Some(Tok::Kw(Kw::From)) => self.parse_new_work(name, line),
            Some(Tok::Kw(Kw::If)) => self.parse_legacy_work(name, line),
            Some(Tok::Ident(s)) => {
                let up = s.to_uppercase();
                if up == "JUDGE" || up == "EXECUTE" || up == "MATCH" {
                    Err(err(
                        line,
                        format!(
                            "`{up}` 為舊提案語法，請改用新子句：JUDGE/讀取→FROM/JOIN、\
                             EXECUTE/目標→INTO、MATCH→JOIN…ON 或頂層 ON + MATCHED/NOT MATCHED"
                        ),
                    ))
                } else {
                    self.parse_legacy_work(name, line)
                }
            }
            _ => Err(err(line, "作業需以 FROM（或舊式 If / 目標表）開始")),
        }
    }

    /// 新子句語法：FROM → JOIN* → WHERE? → INTO → (ON …)? → ADD/…
    fn parse_new_work(&mut self, name: Option<String>, line: usize) -> Result<Work, ScriptIssue> {
        self.scope = Scope::default();

        // FROM
        self.eat(&Tok::Kw(Kw::From), "FROM")?;
        let from = self.parse_binding()?;
        self.scope.from_alias = from.alias.clone();
        self.scope.register(&from.alias, &from.table);

        // JOIN*
        let mut joins = Vec::new();
        loop {
            let policy = match self.peek() {
                Some(Tok::Kw(Kw::Inner)) => {
                    self.next();
                    Some(JoinPolicy::Inner)
                }
                Some(Tok::Kw(Kw::Left)) => {
                    self.next();
                    Some(JoinPolicy::Left)
                }
                _ => None,
            };
            if self.peek() != Some(&Tok::Kw(Kw::Join)) {
                if policy.is_some() {
                    return Err(err(self.line(), "INNER / LEFT 之後預期 JOIN"));
                }
                break;
            }
            self.next(); // JOIN
            let binding = self.parse_binding()?;
            self.scope.register(&binding.alias, &binding.table);
            self.eat(&Tok::Kw(Kw::On), "JOIN 之後的 ON (<條件>)")?;
            let on = self.parse_paren_condition()?;
            joins.push(Join {
                binding,
                on,
                policy: policy.unwrap_or(JoinPolicy::Inner),
            });
        }

        // WHERE?（裸布林條件）
        let where_ = if self.peek() == Some(&Tok::Kw(Kw::Where)) {
            self.next();
            Some(self.parse_or()?)
        } else {
            None
        };

        // INTO
        let into = self.parse_into()?;
        if let Some(a) = &into.alias {
            self.scope.register(a, &into.table);
        }

        // 合併（頂層 ON + MATCHED / NOT MATCHED）
        let merge = if self.peek() == Some(&Tok::Kw(Kw::On)) {
            self.next();
            let on = self.parse_paren_condition()?;
            let mut matched = None;
            let mut not_matched = None;
            loop {
                match self.peek() {
                    Some(Tok::Kw(Kw::Not)) => {
                        self.next();
                        self.eat(&Tok::Kw(Kw::Matched), "NOT 之後的 MATCHED")?;
                        if not_matched.is_some() {
                            return Err(err(self.line(), "重複的 NOT MATCHED 區塊"));
                        }
                        not_matched = Some(self.parse_merge_branch()?);
                    }
                    Some(Tok::Kw(Kw::Matched)) => {
                        self.next();
                        if matched.is_some() {
                            return Err(err(self.line(), "重複的 MATCHED 區塊"));
                        }
                        matched = Some(self.parse_merge_branch()?);
                    }
                    _ => break,
                }
            }
            if matched.is_none() && not_matched.is_none() {
                return Err(err(
                    into.line,
                    "頂層 ON 之後需要至少一個 MATCHED 或 NOT MATCHED 區塊",
                ));
            }
            Some(Merge {
                on,
                matched,
                not_matched,
            })
        } else {
            None
        };

        // 無 merge → 頂層動作
        let action = if merge.is_none() {
            Some(self.parse_action()?)
        } else {
            None
        };

        Ok(Work {
            name,
            from,
            joins,
            where_,
            into,
            merge,
            action,
            line,
        })
    }

    /// 舊式相容：`If L == R 換行 [表] ADD {…}` 或裸 `[表] ADD {…}`，正規化為新 AST。
    fn parse_legacy_work(&mut self, name: Option<String>, line: usize) -> Result<Work, ScriptIssue> {
        // 1. 選擇性 If 條件（先收集 raw parts，待合成綁定後再解析）
        let cond = if self.peek() == Some(&Tok::Kw(Kw::If)) {
            let cline = self.line();
            self.next();
            let (left, _) = self.parse_raw_parts()?;
            self.eat(&Tok::Eq, "If 條件需使用 `==` 比較")?;
            let (right, _) = self.parse_raw_parts()?;
            if right.len() < 2 {
                return Err(err(
                    cline,
                    "`==` 右側必須是資料表欄位（如 [db].[schema].[Table].[Col]）",
                ));
            }
            Some((left, right, cline))
        } else {
            None
        };

        // 2. 目標表 + ADD
        let (table_parts, tline) = self.parse_raw_parts()?;
        if table_parts.len() > 3 {
            return Err(err(tline, "目標資料表最多 3 段（db.schema.table）"));
        }

        // 3. 合成綁定 → 建立舊式作用域
        self.scope = Scope {
            legacy: true,
            ..Scope::default()
        };
        let mut taken: Vec<String> = Vec::new();

        // 主來源（FROM）：若 If 左側帶表路徑則沿用，`[SOURCE]` / 無前綴 → 名目來源
        let (from_table, source_col) = match &cond {
            Some((left, _, _)) => {
                let col = left.last().cloned();
                let tbl: Vec<String> = left[..left.len().saturating_sub(1)].to_vec();
                let tbl = if tbl.iter().all(|s| s.eq_ignore_ascii_case("source")) {
                    Vec::new()
                } else {
                    tbl
                };
                (tbl, col)
            }
            None => (Vec::new(), None),
        };
        let from_alias = derive_alias(&from_table, "src", &taken);
        taken.push(from_alias.clone());
        self.scope.from_alias = from_alias.clone();
        self.scope.register(&from_alias, &from_table);
        let from = Binding {
            alias: from_alias.clone(),
            conn: ConnRef::Source,
            table: from_table,
            line,
        };

        // 查表（JOIN）：If 右側的表路徑
        let mut joins = Vec::new();
        if let Some((_, right, cline)) = &cond {
            let join_col = right.last().cloned().unwrap_or_default();
            let join_table: Vec<String> = right[..right.len() - 1].to_vec();
            let join_alias = derive_alias(&join_table, "lookup", &taken);
            taken.push(join_alias.clone());
            self.scope.register(&join_alias, &join_table);
            let on = Condition::Compare(Comparison {
                left: Expr::Col(ColRef {
                    alias: from_alias.clone(),
                    column: source_col.clone().unwrap_or_default(),
                    line: *cline,
                }),
                op: CmpOp::Eq,
                right: Expr::Col(ColRef {
                    alias: join_alias.clone(),
                    column: join_col,
                    line: *cline,
                }),
                line: *cline,
            });
            joins.push(Join {
                binding: Binding {
                    alias: join_alias,
                    conn: ConnRef::Target,
                    table: join_table,
                    line: *cline,
                },
                on,
                policy: JoinPolicy::Inner,
            });
        }

        // 4. INTO + ADD（指派值以舊式作用域正規化）
        let into = IntoClause {
            conn: ConnRef::Target,
            table: table_parts,
            alias: None,
            line: tline,
        };
        self.eat(&Tok::Kw(Kw::Add), "目標資料表之後預期 ADD")?;
        let assignments = self.parse_assignment_block()?;

        Ok(Work {
            name,
            from,
            joins,
            where_: None,
            into,
            merge: None,
            action: Some(Action::Add(assignments)),
            line,
        })
    }

    /// `WORK '名稱' { <主體> }`。
    fn parse_work_block(&mut self) -> Result<Work, ScriptIssue> {
        let line = self.line();
        self.next(); // WORK
        let name = self.expect_str(line, "WORK 之後預期 '作業名稱'（字串）")?;
        self.eat(&Tok::LBrace, "WORK '名稱' 之後的 `{`")?;
        let work = self.parse_work_body(Some(name), line)?;
        self.eat(&Tok::RBrace, "WORK 區塊結尾 `}`")?;
        Ok(work)
    }

    // ---- 標頭 ----

    fn peek_header_keyword(&self) -> Option<Kw> {
        match (self.peek(), self.peek_at(1)) {
            (Some(Tok::Kw(kw @ (Kw::Source | Kw::Target))), Some(Tok::Assign)) => Some(*kw),
            _ => None,
        }
    }

    fn parse_header(&mut self) -> Result<ScriptHeader, ScriptIssue> {
        let mut header = ScriptHeader::default();
        while let Some(keyword) = self.peek_header_keyword() {
            let line = self.line();
            self.next(); // SOURCE / TARGET
            self.next(); // =
            if keyword == Kw::Source {
                if header.source.is_some() {
                    return Err(err(line, "重複的 SOURCE 宣告"));
                }
                header.source = Some(self.parse_source_decl(line)?);
            } else {
                if header.target_connection.is_some() {
                    return Err(err(line, "重複的 TARGET 宣告"));
                }
                header.target_connection = Some(self.parse_connection_ref(line)?);
            }
        }
        Ok(header)
    }

    fn parse_source_decl(&mut self, line: usize) -> Result<SourceDecl, ScriptIssue> {
        let (func, _) = self.expect_ident("FILE(...) 或 CONNECTION('名稱')")?;
        match func.to_uppercase().as_str() {
            "FILE" => Ok(SourceDecl::File(self.parse_file_args(line)?)),
            "CONNECTION" => Ok(SourceDecl::Connection(
                self.parse_source_connection_args(line)?,
            )),
            other => Err(err(
                line,
                format!("未知的來源型式 {other}（支援 FILE / CONNECTION）"),
            )),
        }
    }

    fn parse_source_connection_args(
        &mut self,
        line: usize,
    ) -> Result<ConnectionSource, ScriptIssue> {
        self.eat(&Tok::LParen, "CONNECTION 之後的 `(`")?;
        let name = self.expect_str(line, "CONNECTION 參數需為字串（'連線名稱'）")?;
        let mut src = ConnectionSource {
            name,
            ..Default::default()
        };
        while self.peek() == Some(&Tok::Comma) {
            self.next();
            let (key, kline) = self.expect_ident("TABLE 或 QUERY")?;
            self.eat(&Tok::Assign, format!("{key} 之後的 `=`").as_str())?;
            match key.to_uppercase().as_str() {
                "TABLE" => {
                    src.table = Some(self.expect_str(kline, "TABLE 需為字串（'schema.table'）")?)
                }
                "QUERY" => {
                    src.query = Some(self.expect_str(kline, "QUERY 需為字串（'SELECT ...'）")?)
                }
                other => return Err(err(kline, format!("未知的 CONNECTION 參數 {other}"))),
            }
        }
        self.eat(&Tok::RParen, "CONNECTION(...) 的 `)`")?;
        if src.table.is_some() && src.query.is_some() {
            return Err(err(line, "TABLE 與 QUERY 只能擇一"));
        }
        Ok(src)
    }

    fn parse_connection_ref(&mut self, line: usize) -> Result<String, ScriptIssue> {
        let (func, _) = self.expect_ident("CONNECTION('已儲存連線名稱')")?;
        if func.to_uppercase() != "CONNECTION" {
            return Err(err(
                line,
                "TARGET 僅支援 CONNECTION('已儲存連線名稱')——密碼存於系統金鑰圈，不寫入腳本",
            ));
        }
        self.eat(&Tok::LParen, "CONNECTION 之後的 `(`")?;
        let name = self.expect_str(line, "CONNECTION 參數需為字串（'連線名稱'）")?;
        self.eat(&Tok::RParen, "CONNECTION('...') 的 `)`")?;
        Ok(name)
    }

    fn parse_file_args(&mut self, line: usize) -> Result<FileSource, ScriptIssue> {
        self.eat(&Tok::LParen, "FILE 之後的 `(`")?;
        let mut fs = FileSource::default();
        let mut first = true;
        loop {
            if self.peek() == Some(&Tok::RParen) {
                self.next();
                break;
            }
            if !first && self.next().map(|t| t.tok) != Some(Tok::Comma) {
                return Err(err(self.line(), "FILE 參數之間需以 `,` 分隔"));
            }
            first = false;
            let (key, kline) =
                self.expect_ident("FILE 參數名稱（PATH / SHEET / ENCODING / HEADER / TYPE）")?;
            self.eat(&Tok::Assign, format!("{key} 之後的 `=`").as_str())?;
            match key.to_uppercase().as_str() {
                // TYPE 僅供可讀性，實際格式依副檔名判斷
                "TYPE" => match self.next().map(|t| t.tok) {
                    Some(Tok::Ident(_)) | Some(Tok::Str(_)) => {}
                    _ => return Err(err(kline, "TYPE 需為格式名稱（如 CSV、XLSX）")),
                },
                "PATH" => fs.path = self.expect_str(kline, "PATH 需為字串（'...'）")?,
                "SHEET" => fs.sheet = Some(self.expect_str(kline, "SHEET 需為字串")?),
                "ENCODING" => {
                    fs.encoding = Some(self.expect_str(kline, "ENCODING 需為字串（如 'Big5'）")?)
                }
                "HEADER" => {
                    fs.has_header = Some(match self.next().map(|t| t.tok) {
                        Some(Tok::Kw(Kw::True)) => true,
                        Some(Tok::Kw(Kw::False)) => false,
                        _ => return Err(err(kline, "HEADER 需為 TRUE / FALSE")),
                    })
                }
                other => return Err(err(kline, format!("未知的 FILE 參數 {other}"))),
            }
        }
        if fs.path.is_empty() {
            return Err(err(line, "FILE(...) 需要 PATH 參數"));
        }
        Ok(fs)
    }
}

/// 扁平化 Concat：把巢狀的 Concat 項展開到同一層，避免 `+` 與模板混用時產生巢狀。
fn push_concat_part(parts: &mut Vec<Expr>, e: Expr) {
    match e {
        Expr::Concat(inner) => parts.extend(inner),
        other => parts.push(other),
    }
}

/// 解析字串模板：固定文字 + `{運算式}` 插值（`{{` / `}}` 跳脫為字面大括號）。
/// 無任何插值時回傳純文字 `Expr::Text`；含插值時回傳 `Expr::Concat`（文字段與插值項交錯）。
fn parse_template(s: &str, line: usize, scope: &Scope) -> Result<Expr, ScriptIssue> {
    let chars: Vec<char> = s.chars().collect();
    let mut parts: Vec<Expr> = Vec::new();
    let mut lit = String::new();
    let mut has_hole = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' if chars.get(i + 1) == Some(&'{') => {
                lit.push('{');
                i += 2;
            }
            '}' if chars.get(i + 1) == Some(&'}') => {
                lit.push('}');
                i += 2;
            }
            '{' => {
                if !lit.is_empty() {
                    parts.push(Expr::Text(std::mem::take(&mut lit)));
                }
                i += 1;
                let mut inner = String::new();
                while i < chars.len() && chars[i] != '}' {
                    inner.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(err(line, "字串插值缺少結尾 }（如需字面大括號請用 {{ }}）"));
                }
                i += 1; // 吃掉 }
                parts.push(parse_hole_expr(&inner, line, scope)?);
                has_hole = true;
            }
            '}' => {
                return Err(err(line, "字串中出現未配對的 }（如需字面大括號請用 {{ }}）"));
            }
            c => {
                lit.push(c);
                i += 1;
            }
        }
    }
    if !has_hole {
        return Ok(Expr::Text(lit));
    }
    if !lit.is_empty() {
        parts.push(Expr::Text(lit));
    }
    Ok(Expr::Concat(parts))
}

/// 解析 `{…}` 插值內容為運算式（欄位參照 / Gen / 數值等）；需完整消耗，並沿用外層別名作用域。
fn parse_hole_expr(inner: &str, line: usize, scope: &Scope) -> Result<Expr, ScriptIssue> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return Err(err(line, "字串插值 {} 內不可為空"));
    }
    let tokens = lex(trimmed)?;
    let mut p = Parser::new(tokens);
    p.scope = scope.clone();
    let expr = p.parse_expr()?;
    if p.peek().is_some() {
        return Err(err(line, format!("字串插值 {{{trimmed}}} 含無法解析的內容")));
    }
    Ok(expr)
}

/// 解析整份腳本：選擇性 SOURCE/TARGET 標頭 + 工作項目。
/// 工作項目為 `WORK '名稱' { … }` 區塊，或舊式以 GO 分隔的裸陳述式；GO 在任何邊界皆可出現（忽略）。
pub fn parse(input: &str) -> Result<Script, ScriptIssue> {
    let tokens = lex(input)?;
    let mut parser = Parser::new(tokens);
    let header = parser.parse_header()?;
    let mut works = Vec::new();

    loop {
        while parser.peek() == Some(&Tok::Kw(Kw::Go)) {
            parser.next();
        }
        if parser.peek().is_none() {
            break;
        }
        if parser.peek() == Some(&Tok::Kw(Kw::Work)) {
            works.push(parser.parse_work_block()?);
        } else {
            let line = parser.line();
            works.push(parser.parse_work_body(None, line)?);
        }
        match parser.peek() {
            None | Some(Tok::Kw(Kw::Go)) | Some(Tok::Kw(Kw::Work)) => {}
            _ => {
                return Err(err(
                    parser.line(),
                    "工作項目結束後預期 WORK、GO 或檔案結尾",
                ));
            }
        }
    }

    if works.is_empty() {
        return Err(err(1, "腳本為空"));
    }
    Ok(Script { header, works })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col<'a>(e: &'a Expr) -> &'a ColRef {
        match e {
            Expr::Col(r) => r,
            _ => panic!("預期欄位參照，得到 {e:?}"),
        }
    }

    #[test]
    fn parses_new_lookup_join_work() {
        let script = parse(
            r#"
WORK 'bind' {
  FROM entra   = SOURCE.[users]
  JOIN account = TARGET.[dbo].[DirectoryAccounts]
    ON (entra.[userPrincipalName] == account.[Email])
  INTO TARGET.[dbo].[ExternalIdentityMappings]
  ADD {
     [Id]                 = Gen.ULID
    ,[AccountId]          = account.[Id]
    ,[ExternalId]         = entra.[id]
    ,[ExternalSystemType] = N'MICROSOFT_ENTRA_ID'
    ,[Label]              = N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'
  }
}
GO
"#,
        )
        .unwrap();
        assert_eq!(script.works.len(), 1);
        let w = &script.works[0];
        assert_eq!(w.name.as_deref(), Some("bind"));
        assert_eq!(w.from.alias, "entra");
        assert_eq!(w.from.conn, ConnRef::Source);
        assert_eq!(w.from.table, vec!["users"]);
        assert_eq!(w.joins.len(), 1);
        let j = &w.joins[0];
        assert_eq!(j.binding.alias, "account");
        assert_eq!(j.binding.conn, ConnRef::Target);
        assert_eq!(j.policy, JoinPolicy::Inner);
        let jon = j.on.as_eq_conjunction().unwrap();
        assert_eq!(jon.len(), 1);
        assert_eq!(col(&jon[0].left).alias, "entra");
        assert_eq!(col(&jon[0].right).alias, "account");
        assert_eq!(w.into.table, vec!["dbo", "ExternalIdentityMappings"]);
        assert!(w.merge.is_none());
        let Some(Action::Add(a)) = &w.action else {
            panic!("預期 ADD");
        };
        assert_eq!(a.len(), 5);
        assert_eq!(a[0].value, Expr::Gen(GenKind::Ulid));
        assert_eq!(col(&a[1].value).alias, "account");
        assert_eq!(col(&a[2].value).alias, "entra");
        // 模板插值內欄位也以別名限定
        let Expr::Concat(parts) = &a[4].value else {
            panic!("預期模板");
        };
        assert!(matches!(&parts[1], Expr::Col(r) if r.alias == "account" && r.column == "DisplayName"));
    }

    #[test]
    fn parses_merge_upsert_work() {
        let script = parse(
            r#"
WORK 'ldap' {
  FROM  account = SOURCE.[dbo].[DirectoryAccounts]
  WHERE account.[LdapId] IS NOT EMPTY
  INTO  mapping = TARGET.[dbo].[ExternalIdentityMappings]
  ON ( mapping.[ExternalSystemType] == N'LDAP' &&
       mapping.[ExternalId]         == account.[LdapId] )
  NOT MATCHED {
    ADD {
       [Id]                 = Gen.ULID
      ,[AccountId]          = account.[Id]
      ,[ExternalSystemType] = N'LDAP'
    }
  }
}
"#,
        )
        .unwrap();
        let w = &script.works[0];
        assert_eq!(w.from.alias, "account");
        assert!(w.joins.is_empty());
        let where_ = w.where_.as_ref().unwrap();
        let Condition::IsEmpty(e) = where_ else {
            panic!("預期 IS NOT EMPTY");
        };
        assert!(e.negated);
        assert_eq!(col(&e.expr).alias, "account");
        assert_eq!(w.into.alias.as_deref(), Some("mapping"));
        let merge = w.merge.as_ref().unwrap();
        let mon = merge.on.as_eq_conjunction().unwrap();
        assert_eq!(mon.len(), 2);
        assert_eq!(col(&mon[0].left).alias, "mapping");
        assert!(matches!(&mon[0].right, Expr::Text(t) if t == "LDAP"));
        assert_eq!(col(&mon[1].right).alias, "account");
        assert!(matches!(merge.not_matched, Some(Action::Add(_))));
        assert!(merge.matched.is_none());
        assert!(w.action.is_none());
    }

    #[test]
    fn legacy_if_add_normalizes_to_new_ast() {
        let script = parse(
            r#"
If [FILENAME].[SHEET1].email == [EluAdminCenter].[dbo].[Account].email
[EluAdminCenter].[dbo].[ExternalIdentityMappings] ADD
{
 AccountId = [EluAdminCenter].[dbo].[Account].Id
,ExternalId = [FILENAME].[SHEET1].Id
,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
}
GO
"#,
        )
        .unwrap();
        let w = &script.works[0];
        // 合成 FROM（來源端表）+ JOIN（查表）
        assert_eq!(w.from.alias, "sheet1");
        assert_eq!(w.from.conn, ConnRef::Source);
        assert_eq!(w.from.table, vec!["FILENAME", "SHEET1"]);
        assert_eq!(w.joins.len(), 1);
        let j = &w.joins[0];
        assert_eq!(j.binding.alias, "account");
        assert_eq!(j.binding.conn, ConnRef::Target);
        assert_eq!(j.binding.table, vec!["EluAdminCenter", "dbo", "Account"]);
        let jon = j.on.as_eq_conjunction().unwrap();
        assert_eq!(col(&jon[0].left).alias, "sheet1");
        assert_eq!(col(&jon[0].left).column, "email");
        assert_eq!(col(&jon[0].right).alias, "account");
        assert_eq!(w.into.table, vec!["EluAdminCenter", "dbo", "ExternalIdentityMappings"]);
        let Some(Action::Add(a)) = &w.action else {
            panic!("預期 ADD");
        };
        // 指派值的表路徑正規化為別名
        assert_eq!(col(&a[0].value).alias, "account"); // [EluAdminCenter].[dbo].[Account].Id
        assert_eq!(col(&a[1].value).alias, "sheet1"); // [FILENAME].[SHEET1].Id
        assert!(matches!(&a[2].value, Expr::Text(t) if t == "MICROSOFT_ENTRA_ID"));
    }

    #[test]
    fn legacy_bare_add_normalizes_to_source_from() {
        let script =
            parse("[users] ADD { name = [f].[s].name, age = 18, ok = TRUE, note = NULL }").unwrap();
        let w = &script.works[0];
        assert!(w.joins.is_empty());
        assert_eq!(w.into.table, vec!["users"]);
        let Some(Action::Add(a)) = &w.action else {
            panic!("預期 ADD");
        };
        // 無 join → 所有欄位歸主來源別名
        assert_eq!(col(&a[0].value).alias, w.from.alias);
        assert_eq!(col(&a[0].value).column, "name");
        assert!(matches!(a[1].value, Expr::Int(18)));
        assert!(matches!(a[2].value, Expr::Bool(true)));
        assert!(matches!(a[3].value, Expr::Null));
    }

    #[test]
    fn left_join_policy_parsed() {
        let script = parse(
            "WORK 'x' { FROM a = SOURCE.[t] LEFT JOIN b = TARGET.[dbo].[T] ON (a.[k] == b.[k]) INTO TARGET.[dbo].[Dst] ADD { [c] = b.[v] } }",
        )
        .unwrap();
        assert_eq!(script.works[0].joins[0].policy, JoinPolicy::Left);
    }

    #[test]
    fn multiple_works_and_go() {
        let script = parse(
            "WORK 'a' { FROM x = SOURCE.[t] INTO TARGET.[d] ADD { [c] = x.[v] } }\nGO\n[u] ADD { y = 2 }\nGO",
        )
        .unwrap();
        assert_eq!(script.works.len(), 2);
        assert_eq!(script.works[0].name.as_deref(), Some("a"));
        assert_eq!(script.works[1].name, None);
    }

    #[test]
    fn unknown_alias_is_error() {
        let e = parse("WORK 'x' { FROM a = SOURCE.[t] INTO TARGET.[d] ADD { [c] = zzz.[v] } }")
            .unwrap_err();
        assert!(e.message.contains("未知別名"));
    }

    #[test]
    fn parses_or_not_in_like_between() {
        let where_of = |src: &str| {
            parse(src).unwrap().works[0].where_.clone().unwrap()
        };
        let pre = "WORK 'x' { FROM a = SOURCE.[t] WHERE ";
        let post = " INTO TARGET.[d] ADD { [c] = a.[v] } }";

        assert!(matches!(
            where_of(&format!("{pre}a.[c] == 1 || a.[c] == 2{post}")),
            Condition::Or(_)
        ));
        assert!(matches!(
            where_of(&format!("{pre}!(a.[c] == 1){post}")),
            Condition::Not(_)
        ));
        assert!(matches!(
            where_of(&format!("{pre}a.[c] IN (1, 2, 3){post}")),
            Condition::In { negated: false, .. }
        ));
        assert!(matches!(
            where_of(&format!("{pre}a.[c] NOT IN (1, 2){post}")),
            Condition::In { negated: true, .. }
        ));
        assert!(matches!(
            where_of(&format!("{pre}a.[name] LIKE N'%x%'{post}")),
            Condition::Like { negated: false, .. }
        ));
        assert!(matches!(
            where_of(&format!("{pre}a.[n] BETWEEN 1 AND 10{post}")),
            Condition::Between { negated: false, .. }
        ));

        // 優先級：a && b || c => Or([And([a,b]), c])
        let c = where_of(&format!("{pre}a.[x] == 1 && a.[y] == 2 || a.[z] == 3{post}"));
        let Condition::Or(terms) = &c else { panic!("預期 Or") };
        assert_eq!(terms.len(), 2);
        assert!(matches!(&terms[0], Condition::And(v) if v.len() == 2));

        // OR / NOT 非等值合取 → JOIN/merge 用的 as_eq_conjunction 回 None
        assert!(where_of(&format!("{pre}a.[c] == 1 || a.[c] == 2{post}"))
            .as_eq_conjunction()
            .is_none());
    }

    #[test]
    fn condition_to_dsl_roundtrips() {
        let where_of = |src: &str| parse(src).unwrap().works[0].where_.clone().unwrap();
        let pre = "WORK 'x' { FROM a = SOURCE.[t] WHERE ";
        let post = " INTO TARGET.[d] ADD { [c] = a.[v] } }";
        // 解析 → to_dsl → 再解析，結構一致
        for cond in [
            "a.[x] == 1 && a.[y] != 2",
            "a.[x] == 1 || a.[y] == 2",
            "(a.[x] == 1 || a.[y] == 2) && a.[z] >= 3",
            "!(a.[x] == 1)",
            "a.[c] NOT IN (1, 2, 3)",
            "a.[name] LIKE N'%x%'",
            "a.[n] BETWEEN 1 AND 10",
        ] {
            let c1 = where_of(&format!("{pre}{cond}{post}"));
            let dsl = c1.to_dsl();
            let c2 = where_of(&format!("{pre}{dsl}{post}"));
            assert_eq!(c1, c2, "round-trip 失敗：{cond} → {dsl}");
        }
    }

    #[test]
    fn parses_matched_update_and_delete() {
        let s = parse(
            r#"WORK 'm' {
                FROM a = SOURCE.[t]
                INTO mapping = TARGET.[dbo].[Dst]
                ON ( mapping.[k] == a.[k] )
                MATCHED { UPDATE { [v] = a.[v] } }
                NOT MATCHED { ADD { [k] = a.[k], [v] = a.[v] } }
            }"#,
        )
        .unwrap();
        let m = s.works[0].merge.as_ref().unwrap();
        assert!(matches!(m.matched, Some(Action::Update(_))));
        assert!(matches!(m.not_matched, Some(Action::Add(_))));

        let s = parse(
            "WORK 'd' { FROM a = SOURCE.[t] INTO mapping = TARGET.[dbo].[Dst] ON ( mapping.[k] == a.[k] ) MATCHED { DELETE } }",
        )
        .unwrap();
        assert!(matches!(
            s.works[0].merge.as_ref().unwrap().matched,
            Some(Action::Delete)
        ));
    }

    #[test]
    fn as_alias_rejected() {
        let e = parse(
            "WORK 'x' { FROM a = SOURCE.[t] INTO TARGET.[d] AS m ADD { [c] = a.[v] } }",
        )
        .unwrap_err();
        assert!(e.message.contains("AS"));
    }

    #[test]
    fn comparison_operators_and_is_empty() {
        let script = parse(
            "WORK 'x' { FROM a = SOURCE.[t] WHERE a.[n] >= 18 && a.[email] IS NOT EMPTY INTO TARGET.[d] ADD { [c] = a.[v] } }",
        )
        .unwrap();
        let w = script.works[0].where_.clone().unwrap();
        let Condition::And(parts) = &w else {
            panic!("預期 AND");
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], Condition::Compare(c) if c.op == CmpOp::Ge));
        assert!(matches!(&parts[1], Condition::IsEmpty(e) if e.negated));
    }

    #[test]
    fn template_roundtrips_alias_field() {
        let script = parse(
            "WORK 'x' { FROM a = SOURCE.[t] INTO TARGET.[d] ADD { [c] = N'L: {a.[Name]}' } }",
        )
        .unwrap();
        let Some(Action::Add(asn)) = &script.works[0].action else {
            panic!();
        };
        assert_eq!(asn[0].value.to_dsl(), "N'L: {a.[Name]}'");
    }

    #[test]
    fn parses_header_and_db_source() {
        let script = parse(
            r#"
SOURCE = CONNECTION('EluCloud')
TARGET = CONNECTION('EluCloud')
WORK 'x' { FROM a = SOURCE.[dbo].[Accounts] INTO TARGET.[dbo].[Dst] ADD { [c] = a.[v] } }
"#,
        )
        .unwrap();
        assert!(matches!(
            script.header.source,
            Some(SourceDecl::Connection(_))
        ));
        assert_eq!(script.header.target_connection.as_deref(), Some("EluCloud"));
        assert_eq!(script.works[0].from.table, vec!["dbo", "Accounts"]);
    }

    #[test]
    fn empty_add_block_parses() {
        let script =
            parse("WORK 'A' { FROM a = SOURCE.[t] INTO TARGET.[d] ADD { } }").unwrap();
        assert!(matches!(
            script.works[0].action,
            Some(Action::Add(ref v)) if v.is_empty()
        ));
    }

    #[test]
    fn reports_line_numbers() {
        let e = parse("[a] ADD\n{ x = }\nGO").unwrap_err();
        assert_eq!(e.line, 2);
    }

    #[test]
    fn string_escapes_and_comments() {
        let script = parse("-- comment\n[t] ADD { a = N'it''s', b = 'x' } GO").unwrap();
        let Some(Action::Add(a)) = &script.works[0].action else {
            panic!();
        };
        assert!(matches!(&a[0].value, Expr::Text(t) if t == "it's"));
    }

    #[test]
    fn parses_generators() {
        let script = parse(
            "WORK 'x' { FROM a = SOURCE.[t] INTO TARGET.[d] ADD { id = Gen.ULID, g = Gen.GUID(Text), d = gen.DateTime, h = Gen.SHA256 } }",
        )
        .unwrap();
        let Some(Action::Add(a)) = &script.works[0].action else {
            panic!();
        };
        assert_eq!(a[0].value, Expr::Gen(GenKind::Ulid));
        assert_eq!(a[1].value, Expr::Gen(GenKind::GuidText));
        assert_eq!(a[2].value, Expr::Gen(GenKind::DateTime));
        assert_eq!(a[3].value, Expr::Gen(GenKind::Sha256));

        let e = parse("[t] ADD { x = Gen.Foo }").unwrap_err();
        assert!(e.message.contains("未知的產生器"));
    }
}
