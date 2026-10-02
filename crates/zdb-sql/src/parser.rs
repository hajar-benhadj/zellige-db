//! AST and hand-written recursive-descent parser.
//!
//! No parser libraries: the parser *is* the exercise. Grammar (v1):
//!
//! ```text
//! statement := create_table | drop_table | insert | select | update
//!            | delete | begin | commit | rollback | show_tables
//! expr      := or_expr (and/or/not, comparisons, arithmetic, LIKE, IS NULL)
//! ```

use crate::lexer::{Symbol, Token, lex};
use crate::types::{DataType, SqlError, Value};

#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable {
        name: String,
        columns: Vec<(String, DataType)>,
    },
    DropTable {
        name: String,
    },
    CreateIndex {
        name: String,
        table: String,
        column: String,
    },
    DropIndex {
        name: String,
    },
    Insert {
        table: String,
        rows: Vec<Vec<Expr>>,
    },
    Select(Select),
    Update {
        table: String,
        assignments: Vec<(String, Expr)>,
        filter: Option<Expr>,
    },
    Delete {
        table: String,
        filter: Option<Expr>,
    },
    Begin,
    Commit,
    Rollback,
    ShowTables,
}

#[derive(Debug, Clone)]
pub struct Select {
    pub items: Vec<SelectItem>,
    pub from: String,
    pub filter: Option<Expr>,
    pub order_by: Vec<Ordering>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum SelectItem {
    Star,
    Column(String),
    CountStar,
}

#[derive(Debug, Clone)]
pub struct Ordering {
    pub column: String,
    pub descending: bool,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Column(String),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Like(Box<Expr>, String),
    IsNull(Box<Expr>, bool), // bool = negated (IS NOT NULL)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

pub fn parse(input: &str) -> Result<Statement, SqlError> {
    let tokens = lex(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let stmt = p.statement()?;
    p.eat_optional_semicolon();
    if p.pos != p.tokens.len() {
        return Err(SqlError::Parse(format!(
            "unexpected trailing input near token {}",
            p.pos + 1
        )));
    }
    Ok(stmt)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(id)) if id.eq_ignore_ascii_case(kw))
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        if self.keyword(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> Result<(), SqlError> {
        if self.eat_keyword(kw) {
            Ok(())
        } else {
            Err(SqlError::Parse(format!("expected keyword {kw}")))
        }
    }

    fn eat_symbol(&mut self, sym: Symbol) -> bool {
        if matches!(self.peek(), Some(Token::Symbol(s)) if *s == sym) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_symbol(&mut self, sym: Symbol) -> Result<(), SqlError> {
        if self.eat_symbol(sym) {
            Ok(())
        } else {
            Err(SqlError::Parse(format!("expected {:?}", sym)))
        }
    }

    fn ident(&mut self) -> Result<String, SqlError> {
        match self.peek() {
            Some(Token::Ident(id)) => {
                let id = id.clone();
                self.pos += 1;
                Ok(id)
            }
            _ => Err(SqlError::Parse("expected identifier".into())),
        }
    }

    fn eat_optional_semicolon(&mut self) {
        while self.eat_symbol(Symbol::Semicolon) {}
    }

    fn statement(&mut self) -> Result<Statement, SqlError> {
        if self.keyword("SELECT") {
            return self.select();
        }
        if self.eat_keyword("CREATE") {
            if self.eat_keyword("INDEX") {
                let name = self.ident()?;
                self.expect_keyword("ON")?;
                let table = self.ident()?;
                self.expect_symbol(Symbol::LParen)?;
                let column = self.ident()?;
                self.expect_symbol(Symbol::RParen)?;
                return Ok(Statement::CreateIndex {
                    name,
                    table,
                    column,
                });
            }
            self.expect_keyword("TABLE")?;
            let name = self.ident()?;
            self.expect_symbol(Symbol::LParen)?;
            let mut columns = Vec::new();
            loop {
                let col = self.ident()?;
                let ty = if self.eat_keyword("INT") || self.eat_keyword("INTEGER") {
                    DataType::Int
                } else if self.eat_keyword("BOOLEAN") || self.eat_keyword("BOOL") {
                    DataType::Bool
                } else if self.eat_keyword("TEXT") || self.eat_keyword("VARCHAR") {
                    DataType::Text
                } else {
                    return Err(SqlError::Parse("expected column type".into()));
                };
                columns.push((col, ty));
                if !self.eat_symbol(Symbol::Comma) {
                    break;
                }
            }
            self.expect_symbol(Symbol::RParen)?;
            return Ok(Statement::CreateTable { name, columns });
        }
        if self.eat_keyword("DROP") {
            if self.eat_keyword("INDEX") {
                return Ok(Statement::DropIndex {
                    name: self.ident()?,
                });
            }
            self.expect_keyword("TABLE")?;
            return Ok(Statement::DropTable {
                name: self.ident()?,
            });
        }
        if self.eat_keyword("INSERT") {
            self.expect_keyword("INTO")?;
            let table = self.ident()?;
            self.expect_keyword("VALUES")?;
            let mut rows = Vec::new();
            loop {
                self.expect_symbol(Symbol::LParen)?;
                let mut row = Vec::new();
                loop {
                    row.push(self.expr()?);
                    if !self.eat_symbol(Symbol::Comma) {
                        break;
                    }
                }
                self.expect_symbol(Symbol::RParen)?;
                rows.push(row);
                if !self.eat_symbol(Symbol::Comma) {
                    break;
                }
            }
            return Ok(Statement::Insert { table, rows });
        }
        if self.eat_keyword("UPDATE") {
            let table = self.ident()?;
            self.expect_keyword("SET")?;
            let mut assignments = Vec::new();
            loop {
                let col = self.ident()?;
                self.expect_symbol(Symbol::Eq)?;
                assignments.push((col, self.expr()?));
                if !self.eat_symbol(Symbol::Comma) {
                    break;
                }
            }
            let filter = if self.eat_keyword("WHERE") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Statement::Update {
                table,
                assignments,
                filter,
            });
        }
        if self.eat_keyword("DELETE") {
            self.expect_keyword("FROM")?;
            let table = self.ident()?;
            let filter = if self.eat_keyword("WHERE") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Statement::Delete { table, filter });
        }
        if self.eat_keyword("BEGIN") {
            return Ok(Statement::Begin);
        }
        if self.eat_keyword("COMMIT") {
            return Ok(Statement::Commit);
        }
        if self.eat_keyword("ROLLBACK") {
            return Ok(Statement::Rollback);
        }
        if self.keyword("SHOW") {
            self.pos += 1;
            self.expect_keyword("TABLES")?;
            return Ok(Statement::ShowTables);
        }
        Err(SqlError::Parse("expected a statement".into()))
    }

    fn select(&mut self) -> Result<Statement, SqlError> {
        self.expect_keyword("SELECT")?;
        let mut items = Vec::new();
        if self.eat_symbol(Symbol::Star) {
            items.push(SelectItem::Star);
        } else if self.keyword("COUNT")
            && matches!(
                self.tokens.get(self.pos + 1),
                Some(Token::Symbol(Symbol::LParen))
            )
            && matches!(
                self.tokens.get(self.pos + 2),
                Some(Token::Symbol(Symbol::Star))
            )
        {
            self.pos += 3;
            self.expect_symbol(Symbol::RParen)?;
            items.push(SelectItem::CountStar);
        } else {
            loop {
                items.push(SelectItem::Column(self.ident()?));
                if !self.eat_symbol(Symbol::Comma) {
                    break;
                }
            }
        }
        self.expect_keyword("FROM")?;
        let from = self.ident()?;
        let filter = if self.eat_keyword("WHERE") {
            Some(self.expr()?)
        } else {
            None
        };
        let mut order_by = Vec::new();
        if self.eat_keyword("ORDER") {
            self.expect_keyword("BY")?;
            loop {
                let column = self.ident()?;
                let descending = if self.eat_keyword("DESC") {
                    true
                } else {
                    self.eat_keyword("ASC");
                    false
                };
                order_by.push(Ordering { column, descending });
                if !self.eat_symbol(Symbol::Comma) {
                    break;
                }
            }
        };
        let limit = if self.eat_keyword("LIMIT") {
            match self.peek() {
                Some(Token::Number(n)) if *n >= 0 => {
                    let n = *n as u64;
                    self.pos += 1;
                    Some(n)
                }
                _ => return Err(SqlError::Parse("expected LIMIT number".into())),
            }
        } else {
            None
        };
        Ok(Statement::Select(Select {
            items,
            from,
            filter,
            order_by,
            limit,
        }))
    }

    // expression grammar: OR < AND < NOT < comparison < additive < multiplicative < unary < primary
    fn expr(&mut self) -> Result<Expr, SqlError> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.and_expr()?;
        while self.eat_keyword("OR") {
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(self.and_expr()?));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.not_expr()?;
        while self.eat_keyword("AND") {
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(self.not_expr()?));
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Expr, SqlError> {
        if self.eat_keyword("NOT") {
            Ok(Expr::Not(Box::new(self.not_expr()?)))
        } else {
            self.comparison()
        }
    }

    fn comparison(&mut self) -> Result<Expr, SqlError> {
        let left = self.additive()?;
        let op = match self.peek() {
            Some(Token::Symbol(Symbol::Eq)) => Some(BinOp::Eq),
            Some(Token::Symbol(Symbol::Ne)) => Some(BinOp::Ne),
            Some(Token::Symbol(Symbol::Lt)) => Some(BinOp::Lt),
            Some(Token::Symbol(Symbol::Le)) => Some(BinOp::Le),
            Some(Token::Symbol(Symbol::Gt)) => Some(BinOp::Gt),
            Some(Token::Symbol(Symbol::Ge)) => Some(BinOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            return Ok(Expr::Binary(op, Box::new(left), Box::new(self.additive()?)));
        }
        if self.keyword("LIKE") {
            self.pos += 1;
            return match self.peek() {
                Some(Token::Str(pattern)) => {
                    let pattern = pattern.clone();
                    self.pos += 1;
                    Ok(Expr::Like(Box::new(left), pattern))
                }
                _ => Err(SqlError::Parse("expected LIKE pattern string".into())),
            };
        }
        if self.keyword("IS") {
            self.pos += 1;
            let negated = self.eat_keyword("NOT");
            self.expect_keyword("NULL")?;
            return Ok(Expr::IsNull(Box::new(left), negated));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Symbol(Symbol::Plus)) => BinOp::Add,
                Some(Token::Symbol(Symbol::Minus)) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            left = Expr::Binary(op, Box::new(left), Box::new(self.multiplicative()?));
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Symbol(Symbol::Star)) => BinOp::Mul,
                Some(Token::Symbol(Symbol::Slash)) => BinOp::Div,
                Some(Token::Symbol(Symbol::Percent)) => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            left = Expr::Binary(op, Box::new(left), Box::new(self.unary()?));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, SqlError> {
        if matches!(self.peek(), Some(Token::Symbol(Symbol::Minus))) {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, SqlError> {
        match self.peek() {
            Some(Token::Number(n)) => {
                let n = *n;
                self.pos += 1;
                Ok(Expr::Literal(Value::Int(n)))
            }
            Some(Token::Str(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(Expr::Literal(Value::Text(s)))
            }
            Some(Token::Ident(id)) if id.eq_ignore_ascii_case("NULL") => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Null))
            }
            Some(Token::Ident(id)) if id.eq_ignore_ascii_case("TRUE") => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Bool(true)))
            }
            Some(Token::Ident(id)) if id.eq_ignore_ascii_case("FALSE") => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Bool(false)))
            }
            Some(Token::Ident(_)) => {
                let name = self.ident()?;
                Ok(Expr::Column(name))
            }
            Some(Token::Symbol(Symbol::LParen)) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect_symbol(Symbol::RParen)?;
                Ok(e)
            }
            _ => Err(SqlError::Parse("expected an expression".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_select_with_all_clauses() {
        let stmt = parse("SELECT name, age FROM users WHERE age > 18 AND name LIKE 'a%' ORDER BY name DESC LIMIT 10;").unwrap();
        match stmt {
            Statement::Select(s) => {
                assert_eq!(s.items.len(), 2);
                assert_eq!(s.from, "users");
                assert!(s.filter.is_some());
                assert_eq!(s.order_by.len(), 1);
                assert_eq!(s.order_by[0].column, "name");
                assert!(s.order_by[0].descending);
                assert_eq!(s.limit, Some(10));
            }
            _ => panic!("wrong statement"),
        }
    }

    #[test]
    fn parses_count_star() {
        let stmt = parse("SELECT COUNT(*) FROM t").unwrap();
        assert!(matches!(
            stmt,
            Statement::Select(Select { items, .. })
                if items.len() == 1 && matches!(items[0], SelectItem::CountStar)
        ));
    }

    #[test]
    fn parses_multi_row_insert() {
        let stmt = parse("INSERT INTO t VALUES (1, 'a', TRUE), (2, NULL, FALSE)").unwrap();
        match stmt {
            Statement::Insert { table, rows } => {
                assert_eq!(table, "t");
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("wrong statement"),
        }
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse("SELECT 1 FROM t SELECT 2 FROM u").is_err());
    }

    #[test]
    fn rejects_unterminated_string() {
        assert!(parse("INSERT INTO t VALUES ('oops)").is_err());
    }
}
