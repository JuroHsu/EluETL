//! ETL 腳本 DSL 的手寫 lexer + recursive descent parser。
//! 關鍵字不分大小寫；`--` 為單行註解；識別字可用 `[名稱]` 或裸字。

use crate::etl::script::ast::{
    Assignment, ColRef, Condition, Expr, Script, ScriptIssue, Statement,
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
    Assign,
    EqEq,
    KwIf,
    KwAdd,
    KwGo,
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

    fn parse_expr(&mut self) -> Result<Expr, ScriptIssue> {
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
            Some(Tok::Ident(_)) => Ok(Expr::Col(self.parse_colref()?)),
            _ => Err(err(line, "預期欄位參照或字面值（'文字'、數字、NULL）")),
        }
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
            condition,
            target_table,
            assignments,
            line: stmt_line,
        })
    }
}

/// 解析整份腳本（陳述式以 GO 分隔；結尾 GO 可省略）。
pub fn parse(input: &str) -> Result<Script, ScriptIssue> {
    let tokens = lex(input)?;
    let mut parser = Parser { tokens, pos: 0 };
    let mut statements = Vec::new();

    loop {
        while parser.peek() == Some(&Tok::KwGo) {
            parser.next();
        }
        if parser.peek().is_none() {
            break;
        }
        statements.push(parser.parse_statement()?);
        match parser.peek() {
            None | Some(Tok::KwGo) => {}
            _ => {
                return Err(err(parser.line(), "陳述式結束後預期 GO 或檔案結尾"));
            }
        }
    }

    if statements.is_empty() {
        return Err(err(1, "腳本為空"));
    }
    Ok(Script { statements })
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
    }

    #[test]
    fn reports_line_numbers() {
        let e = parse("[a] ADD\n{ x = }\nGO").unwrap_err();
        assert_eq!(e.line, 2);

        let e = parse("If a == b\n[t] ADD { x = 1 }").unwrap_err();
        assert!(e.message.contains("右側"));
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
