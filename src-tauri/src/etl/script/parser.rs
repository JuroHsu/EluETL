//! ETL 腳本 DSL 的手寫 lexer + recursive descent parser。
//! 關鍵字不分大小寫；`--` 為單行註解；識別字可用 `[名稱]` 或裸字。

use crate::etl::script::ast::{
    Assignment, ColRef, Condition, ConnectionSource, Expr, FileSource, GenKind, Script,
    ScriptHeader, ScriptIssue, SourceDecl, Statement,
};

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
    Assign,
    EqEq,
    KwIf,
    KwAdd,
    KwGo,
    KwWork,
    KwNull,
    KwTrue,
    KwFalse,
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

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => {
                // 單行註解
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
            '=' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token {
                    tok: Tok::EqEq,
                    line,
                });
                i += 2;
            }
            '=' => {
                tokens.push(Token {
                    tok: Tok::Assign,
                    line,
                });
                i += 1;
            }
            '.' => {
                tokens.push(Token {
                    tok: Tok::Dot,
                    line,
                });
                i += 1;
            }
            ',' => {
                tokens.push(Token {
                    tok: Tok::Comma,
                    line,
                });
                i += 1;
            }
            '{' => {
                tokens.push(Token {
                    tok: Tok::LBrace,
                    line,
                });
                i += 1;
            }
            '}' => {
                tokens.push(Token {
                    tok: Tok::RBrace,
                    line,
                });
                i += 1;
            }
            '(' => {
                tokens.push(Token {
                    tok: Tok::LParen,
                    line,
                });
                i += 1;
            }
            ')' => {
                tokens.push(Token {
                    tok: Tok::RParen,
                    line,
                });
                i += 1;
            }
            '+' => {
                tokens.push(Token {
                    tok: Tok::Plus,
                    line,
                });
                i += 1;
            }
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
                    "IF" => Tok::KwIf,
                    "ADD" => Tok::KwAdd,
                    "GO" => Tok::KwGo,
                    "WORK" => Tok::KwWork,
                    "NULL" => Tok::KwNull,
                    "TRUE" => Tok::KwTrue,
                    "FALSE" => Tok::KwFalse,
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

// ---------- Parser ----------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
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

    /// `a.b.c` → ColRef（至少 1 段）
    fn parse_colref(&mut self) -> Result<ColRef, ScriptIssue> {
        let (first, line) = self.expect_ident("識別字")?;
        let mut parts = vec![first];
        while self.peek() == Some(&Tok::Dot) {
            self.next();
            let (p, _) = self.expect_ident("`.` 之後的識別字")?;
            parts.push(p);
        }
        let column = parts.pop().unwrap();
        Ok(ColRef {
            prefix: parts,
            column,
            line,
        })
    }

    /// 運算式：單一項，或以 `+` 串接的合成欄位（轉文字後串接）。
    fn parse_expr(&mut self) -> Result<Expr, ScriptIssue> {
        let first = self.parse_term()?;
        if self.peek() != Some(&Tok::Plus) {
            return Ok(first);
        }
        let mut parts = vec![first];
        while self.peek() == Some(&Tok::Plus) {
            self.next();
            parts.push(self.parse_term()?);
        }
        Ok(Expr::Concat(parts))
    }

    fn parse_term(&mut self) -> Result<Expr, ScriptIssue> {
        let line = self.line();
        match self.peek() {
            Some(Tok::Str(_)) => {
                let Some(Token {
                    tok: Tok::Str(s), ..
                }) = self.next()
                else {
                    unreachable!()
                };
                Ok(Expr::Text(s))
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
            Some(Tok::KwNull) => {
                self.next();
                Ok(Expr::Null)
            }
            Some(Tok::KwTrue) => {
                self.next();
                Ok(Expr::Bool(true))
            }
            Some(Tok::KwFalse) => {
                self.next();
                Ok(Expr::Bool(false))
            }
            Some(Tok::Ident(_)) => {
                let r = self.parse_colref()?;
                if r.prefix.len() == 1 && r.prefix[0].eq_ignore_ascii_case("gen") {
                    return self.parse_gen(r);
                }
                Ok(Expr::Col(r))
            }
            _ => Err(err(line, "預期欄位參照或字面值（'文字'、數字、NULL）")),
        }
    }

    /// `Gen.XXX` / `Gen.XXX(Text)` 產生器（Gen 為保留前綴）。
    fn parse_gen(&mut self, r: ColRef) -> Result<Expr, ScriptIssue> {
        let mut text_variant = false;
        if self.peek() == Some(&Tok::LParen) {
            self.next();
            match self.next() {
                Some(Token {
                    tok: Tok::Ident(s), ..
                }) if s.eq_ignore_ascii_case("text") => {}
                _ => return Err(err(r.line, "產生器參數僅支援 (Text)")),
            }
            if self.next().map(|t| t.tok) != Some(Tok::RParen) {
                return Err(err(r.line, "產生器缺少 `)`"));
            }
            text_variant = true;
        }
        GenKind::parse(&r.column, text_variant)
            .map(Expr::Gen)
            .ok_or_else(|| {
                err(
                    r.line,
                    format!(
                        "未知的產生器 Gen.{}{}（支援：{}）",
                        r.column,
                        if text_variant { "(Text)" } else { "" },
                        GenKind::ALL_LABELS
                    ),
                )
            })
    }

    fn expect_str(&mut self, line: usize, what: &str) -> Result<String, ScriptIssue> {
        match self.next() {
            Some(Token {
                tok: Tok::Str(s), ..
            }) => Ok(s),
            _ => Err(err(line, what.to_string())),
        }
    }

    /// 下兩個 token 是否為 `SOURCE =` / `TARGET =` 標頭開頭。
    fn peek_header_keyword(&self) -> Option<String> {
        match (self.peek(), self.peek_at(1)) {
            (Some(Tok::Ident(s)), Some(Tok::Assign)) => {
                let up = s.to_uppercase();
                (up == "SOURCE" || up == "TARGET").then_some(up)
            }
            _ => None,
        }
    }

    /// 標頭區：`SOURCE = FILE(...)` / `SOURCE = CONNECTION('名稱')` /
    /// `TARGET = CONNECTION('名稱')`，順序不拘，皆為選擇性。
    fn parse_header(&mut self) -> Result<ScriptHeader, ScriptIssue> {
        let mut header = ScriptHeader::default();
        while let Some(keyword) = self.peek_header_keyword() {
            let line = self.line();
            self.next(); // SOURCE / TARGET
            self.next(); // =
            if keyword == "SOURCE" {
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

    /// SOURCE 的 CONNECTION 參數：`('名稱' [, TABLE='…' | QUERY='…'])`。
    /// 檔案連線只用名稱；資料庫連線以 TABLE / QUERY 指明讀取內容（擇一）。
    fn parse_source_connection_args(
        &mut self,
        line: usize,
    ) -> Result<ConnectionSource, ScriptIssue> {
        if self.next().map(|t| t.tok) != Some(Tok::LParen) {
            return Err(err(line, "CONNECTION 之後預期 `(`"));
        }
        let name = self.expect_str(line, "CONNECTION 參數需為字串（'連線名稱'）")?;
        let mut src = ConnectionSource {
            name,
            ..Default::default()
        };
        while self.peek() == Some(&Tok::Comma) {
            self.next();
            let (key, kline) = self.expect_ident("TABLE 或 QUERY")?;
            if self.next().map(|t| t.tok) != Some(Tok::Assign) {
                return Err(err(kline, format!("{key} 之後預期 `=`")));
            }
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
        if self.next().map(|t| t.tok) != Some(Tok::RParen) {
            return Err(err(line, "CONNECTION(...) 缺少 `)`"));
        }
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
        self.parse_connection_args(line)
    }

    fn parse_connection_args(&mut self, line: usize) -> Result<String, ScriptIssue> {
        if self.next().map(|t| t.tok) != Some(Tok::LParen) {
            return Err(err(line, "CONNECTION 之後預期 `(`"));
        }
        let name = self.expect_str(line, "CONNECTION 參數需為字串（'連線名稱'）")?;
        if self.next().map(|t| t.tok) != Some(Tok::RParen) {
            return Err(err(line, "CONNECTION('...') 缺少 `)`"));
        }
        Ok(name)
    }

    fn parse_file_args(&mut self, line: usize) -> Result<FileSource, ScriptIssue> {
        if self.next().map(|t| t.tok) != Some(Tok::LParen) {
            return Err(err(line, "FILE 之後預期 `(`"));
        }
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
            if self.next().map(|t| t.tok) != Some(Tok::Assign) {
                return Err(err(kline, format!("{key} 之後預期 `=`")));
            }
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
                        Some(Tok::KwTrue) => true,
                        Some(Tok::KwFalse) => false,
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

    fn parse_statement(&mut self) -> Result<Statement, ScriptIssue> {
        let stmt_line = self.line();

        // 選擇性 IF 條件
        let condition = if self.peek() == Some(&Tok::KwIf) {
            let line = self.line();
            self.next();
            let left = self.parse_colref()?;
            if self.next().map(|t| t.tok) != Some(Tok::EqEq) {
                return Err(err(line, "IF 條件需使用 `==` 比較"));
            }
            let right = self.parse_colref()?;
            if right.prefix.is_empty() {
                return Err(err(
                    line,
                    "`==` 右側必須是資料表欄位（如 [db].[schema].[Table].Col）",
                ));
            }
            Some(Condition { left, right, line })
        } else {
            None
        };

        // 目標資料表（1..3 段）+ ADD
        let table_ref = self.parse_colref()?;
        let mut target_table = table_ref.prefix;
        target_table.push(table_ref.column);
        if target_table.len() > 3 {
            return Err(err(
                table_ref.line,
                "目標資料表最多 3 段（db.schema.table）",
            ));
        }
        if self.next().map(|t| t.tok) != Some(Tok::KwAdd) {
            return Err(err(
                table_ref.line,
                "目標資料表之後預期 ADD（目前僅支援新增列）",
            ));
        }

        // { 欄位 = 值, ... }
        if self.next().map(|t| t.tok) != Some(Tok::LBrace) {
            return Err(err(self.line(), "ADD 之後預期 `{`"));
        }
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
            if self.next().map(|t| t.tok) != Some(Tok::Assign) {
                return Err(err(line, format!("欄位 {target_column} 之後預期 `=`")));
            }
            let value = self.parse_expr()?;
            assignments.push(Assignment {
                target_column,
                value,
                line,
            });
        }
        if assignments.is_empty() {
            return Err(err(stmt_line, "至少需要一個欄位指派"));
        }

        Ok(Statement {
            name: None,
            condition,
            target_table,
            assignments,
            line: stmt_line,
        })
    }

    /// `WORK '名稱' { <陳述式> }` 工作項目區塊。
    fn parse_work(&mut self) -> Result<Statement, ScriptIssue> {
        let line = self.line();
        self.next(); // WORK
        let name = self.expect_str(line, "WORK 之後預期 '作業名稱'（字串）")?;
        if self.next().map(|t| t.tok) != Some(Tok::LBrace) {
            return Err(err(line, "WORK '名稱' 之後預期 `{`"));
        }
        let mut stmt = self.parse_statement()?;
        stmt.name = Some(name);
        if self.next().map(|t| t.tok) != Some(Tok::RBrace) {
            return Err(err(self.line(), "WORK 區塊缺少結尾 `}`"));
        }
        Ok(stmt)
    }
}

/// 解析整份腳本：選擇性 SOURCE/TARGET 標頭 + 工作項目。
/// 工作項目為 `WORK '名稱' { … }` 區塊（之間不需 GO），
/// 或舊式以 GO 分隔的裸陳述式；GO 在任何項目邊界皆可出現（忽略）。
pub fn parse(input: &str) -> Result<Script, ScriptIssue> {
    let tokens = lex(input)?;
    let mut parser = Parser { tokens, pos: 0 };
    let header = parser.parse_header()?;
    let mut statements = Vec::new();

    loop {
        while parser.peek() == Some(&Tok::KwGo) {
            parser.next();
        }
        if parser.peek().is_none() {
            break;
        }
        if parser.peek() == Some(&Tok::KwWork) {
            statements.push(parser.parse_work()?);
        } else {
            statements.push(parser.parse_statement()?);
        }
        match parser.peek() {
            None | Some(Tok::KwGo) | Some(Tok::KwWork) => {}
            _ => {
                return Err(err(parser.line(), "工作項目結束後預期 WORK、GO 或檔案結尾"));
            }
        }
    }

    if statements.is_empty() {
        return Err(err(1, "腳本為空"));
    }
    Ok(Script { header, statements })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
-- 將來源 email 對應到既有帳號，寫入外部身分對應表
If [FILENAME].[SHEET1].email == [EluAdminCenter].[dbo].[Account].email
[EluAdminCenter].[dbo].[ExternalIdentityMappings] ADD
{
 AccountId = [EluAdminCenter].[dbo].[Account].Id
,ExternalId = [FILENAME].[SHEET1].Id
,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
}
GO
"#;

    #[test]
    fn parses_user_sample() {
        let script = parse(SAMPLE).unwrap();
        assert_eq!(script.statements.len(), 1);
        let s = &script.statements[0];

        let cond = s.condition.as_ref().unwrap();
        assert_eq!(cond.left.prefix, vec!["FILENAME", "SHEET1"]);
        assert_eq!(cond.left.column, "email");
        assert_eq!(cond.right.prefix, vec!["EluAdminCenter", "dbo", "Account"]);
        assert_eq!(cond.right.column, "email");

        assert_eq!(
            s.target_table,
            vec!["EluAdminCenter", "dbo", "ExternalIdentityMappings"]
        );
        assert_eq!(s.assignments.len(), 3);
        assert_eq!(s.assignments[0].target_column, "AccountId");
        assert!(matches!(
            &s.assignments[2].value,
            Expr::Text(t) if t == "MICROSOFT_ENTRA_ID"
        ));
    }

    #[test]
    fn parses_without_condition_and_literals() {
        let script = parse(
            "[users] ADD { name = [f].[s].name, age = 18, score = 1.5, ok = TRUE, note = NULL }",
        )
        .unwrap();
        let s = &script.statements[0];
        assert!(s.condition.is_none());
        assert_eq!(s.target_table, vec!["users"]);
        assert_eq!(s.assignments.len(), 5);
        assert!(matches!(s.assignments[1].value, Expr::Int(18)));
        assert!(matches!(s.assignments[2].value, Expr::Float(_)));
        assert!(matches!(s.assignments[3].value, Expr::Bool(true)));
        assert!(matches!(s.assignments[4].value, Expr::Null));
    }

    #[test]
    fn multiple_statements_with_go() {
        let script = parse("[a] ADD { x = 1 }\nGO\n[b] ADD { y = 2 }\nGO").unwrap();
        assert_eq!(script.statements.len(), 2);
        assert_eq!(script.statements[0].name, None);
    }

    #[test]
    fn parses_work_blocks() {
        // 使用者範例格式：WORK 區塊之間不需 GO，結尾 GO 可有可無；名稱可重複
        let script = parse(
            r#"
SOURCE = FILE(TYPE=CSV, PATH='C:\data\users.csv', ENCODING='Big5', HEADER=TRUE)
TARGET = CONNECTION('正式環境 ERP')
WORK 'EluCloudAccount綁定EnterId' {
If [SOURCE].email == [dbo].[Account].email
[dbo].[ExternalIdentityMappings] ADD
{
 AccountId = [dbo].[Account].Id
,ExternalId = [SOURCE].Id
,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
}
}
WORK 'EluCloudAccount綁定EnterId' {
If [SOURCE].email == [dbo].[Account].email
[dbo].[ExternalIdentityMappings] ADD
{
 AccountId = [dbo].[Account].Id
,ExternalId = [SOURCE].Id
,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
}
}

GO
"#,
        )
        .unwrap();
        assert_eq!(script.statements.len(), 2);
        assert_eq!(
            script.statements[0].name.as_deref(),
            Some("EluCloudAccount綁定EnterId")
        );
        assert_eq!(
            script.statements[1].name.as_deref(),
            Some("EluCloudAccount綁定EnterId")
        );
        let s = &script.statements[0];
        assert!(s.condition.is_some());
        assert_eq!(s.target_table, vec!["dbo", "ExternalIdentityMappings"]);
        assert_eq!(s.assignments.len(), 3);
    }

    #[test]
    fn work_block_mixed_with_legacy_statement() {
        let script = parse("WORK 'A' { [t] ADD { x = 1 } }\nGO\n[u] ADD { y = 2 }\nGO").unwrap();
        assert_eq!(script.statements.len(), 2);
        assert_eq!(script.statements[0].name.as_deref(), Some("A"));
        assert_eq!(script.statements[1].name, None);
    }

    #[test]
    fn parses_generators() {
        let script = parse(
            "[t] ADD { id = Gen.ULID, g = Gen.GUID(Text), d = gen.DateTime, h = Gen.SHA256 }",
        )
        .unwrap();
        let a = &script.statements[0].assignments;
        assert_eq!(a[0].value, Expr::Gen(GenKind::Ulid));
        assert_eq!(a[1].value, Expr::Gen(GenKind::GuidText));
        assert_eq!(a[2].value, Expr::Gen(GenKind::DateTime));
        assert_eq!(a[3].value, Expr::Gen(GenKind::Sha256));

        // 未知產生器 / 不支援的 (Text) 變體
        let e = parse("[t] ADD { x = Gen.Foo }").unwrap_err();
        assert!(e.message.contains("未知的產生器"));
        let e = parse("[t] ADD { x = Gen.ULID(Text) }").unwrap_err();
        assert!(e.message.contains("未知的產生器"));
    }

    #[test]
    fn parses_concat_expressions() {
        // 使用者範例：常值 + 比對表欄位、來源欄位 + 常值
        let script = parse(
            "[t] ADD {
               a = N'MICROSOFT_ENTRA_ID:' + [dbo].[DirectoryAccounts].[DisplayName],
               b = SOURCE.displayName + N'MICROSOFT_ENTRA_ID',
               c = N'x' + Gen.GUID + 1
             }",
        )
        .unwrap();
        let a = &script.statements[0].assignments;

        let Expr::Concat(parts) = &a[0].value else {
            panic!("預期合成欄位");
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], Expr::Text(t) if t == "MICROSOFT_ENTRA_ID:"));
        assert!(matches!(&parts[1], Expr::Col(r) if r.column == "DisplayName"));
        assert_eq!(
            a[0].value.to_dsl(),
            "N'MICROSOFT_ENTRA_ID:' + [dbo].[DirectoryAccounts].[DisplayName]"
        );

        let Expr::Concat(parts) = &a[1].value else {
            panic!("預期合成欄位");
        };
        assert!(matches!(&parts[0], Expr::Col(r) if r.prefix == vec!["SOURCE"]));

        let Expr::Concat(parts) = &a[2].value else {
            panic!("預期合成欄位");
        };
        assert_eq!(parts.len(), 3);
        assert!(matches!(parts[1], Expr::Gen(GenKind::Guid)));
        assert!(matches!(parts[2], Expr::Int(1)));

        // 尾隨 + 缺項 → 錯誤
        assert!(parse("[t] ADD { x = N'a' + }").is_err());
    }

    #[test]
    fn work_block_errors() {
        // 名稱必須是字串
        let e = parse("WORK abc { [t] ADD { x = 1 } }").unwrap_err();
        assert!(e.message.contains("作業名稱"));
        // 缺少結尾 }
        let e = parse("WORK 'A' { [t] ADD { x = 1 }").unwrap_err();
        assert!(e.message.contains("結尾"));
        // 缺少 {
        let e = parse("WORK 'A' [t] ADD { x = 1 } }").unwrap_err();
        assert!(e.message.contains("`{`"));
    }

    #[test]
    fn reports_line_numbers() {
        let e = parse("[a] ADD\n{ x = }\nGO").unwrap_err();
        assert_eq!(e.line, 2);

        let e = parse("If a == b\n[t] ADD { x = 1 }").unwrap_err();
        assert!(e.message.contains("右側"));
    }

    #[test]
    fn parses_source_target_header() {
        let script = parse(
            r#"
SOURCE = FILE(TYPE=CSV, PATH='C:\data\users.csv', SHEET='SHEET1', ENCODING='Big5', HEADER=TRUE)
TARGET = CONNECTION('正式環境 ERP')

[t] ADD { x = 1 }
GO
"#,
        )
        .unwrap();
        let Some(SourceDecl::File(f)) = &script.header.source else {
            panic!("expected file source");
        };
        assert_eq!(f.path, r"C:\data\users.csv");
        assert_eq!(f.sheet.as_deref(), Some("SHEET1"));
        assert_eq!(f.encoding.as_deref(), Some("Big5"));
        assert_eq!(f.has_header, Some(true));
        assert_eq!(
            script.header.target_connection.as_deref(),
            Some("正式環境 ERP")
        );
    }

    #[test]
    fn parses_source_connection_ref_and_no_header() {
        let script = parse("SOURCE = CONNECTION('月結檔')\n[t] ADD { x = 1 }").unwrap();
        assert_eq!(
            script.header.source,
            Some(SourceDecl::Connection(ConnectionSource {
                name: "月結檔".into(),
                table: None,
                query: None,
            }))
        );
        assert!(script.header.target_connection.is_none());

        // 無標頭照常解析
        let script = parse("[t] ADD { x = 1 }").unwrap();
        assert!(script.header.source.is_none());
    }

    #[test]
    fn parses_db_source_connection_with_table_or_query() {
        let script =
            parse("SOURCE = CONNECTION('來源DB', TABLE='dbo.users')\n[t] ADD { x = 1 }").unwrap();
        assert_eq!(
            script.header.source,
            Some(SourceDecl::Connection(ConnectionSource {
                name: "來源DB".into(),
                table: Some("dbo.users".into()),
                query: None,
            }))
        );

        let script = parse(
            "SOURCE = CONNECTION('來源DB', QUERY='SELECT id, email FROM users WHERE active = 1')\n[t] ADD { x = 1 }",
        )
        .unwrap();
        assert_eq!(
            script.header.source,
            Some(SourceDecl::Connection(ConnectionSource {
                name: "來源DB".into(),
                table: None,
                query: Some("SELECT id, email FROM users WHERE active = 1".into()),
            }))
        );

        // TABLE 與 QUERY 不可同時指定
        let e =
            parse("SOURCE = CONNECTION('x', TABLE='a', QUERY='b')\n[t] ADD { x = 1 }").unwrap_err();
        assert!(e.message.contains("擇一"));
    }

    #[test]
    fn header_errors() {
        // TARGET 不接受 FILE（目標必須是資料庫連線）
        let e = parse("TARGET = FILE(PATH='x')\n[t] ADD { x = 1 }").unwrap_err();
        assert!(e.message.contains("CONNECTION"));
        // FILE 缺 PATH
        let e = parse("SOURCE = FILE(TYPE=CSV)\n[t] ADD { x = 1 }").unwrap_err();
        assert!(e.message.contains("PATH"));
    }

    #[test]
    fn string_escapes_and_comments() {
        let script = parse("-- comment\n[t] ADD { a = N'it''s', b = 'x' } GO").unwrap();
        assert!(matches!(
            &script.statements[0].assignments[0].value,
            Expr::Text(t) if t == "it's"
        ));
    }
}
