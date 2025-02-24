use std::collections::HashMap;

use crate::ast::*;
use crate::lexer::Token;
use crate::CypherError;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Statement, CypherError> {
        let stmt = match self.peek() {
            Token::Match => self.parse_match()?,
            Token::Create => self.parse_create()?,
            Token::Merge => self.parse_merge()?,
            _ => return Err(self.error("expected MATCH, CREATE, or MERGE")),
        };
        self.expect(Token::Eof)?;
        Ok(stmt)
    }

    // ---- helpers ----

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: Token) -> Result<(), CypherError> {
        let tok = self.advance();
        if std::mem::discriminant(&tok) == std::mem::discriminant(&expected) {
            Ok(())
        } else {
            Err(self.error(&format!("expected {:?}, got {:?}", expected, tok)))
        }
    }

    fn expect_ident(&mut self) -> Result<String, CypherError> {
        match self.advance() {
            Token::Ident(s) => Ok(s),
            other => Err(self.error(&format!("expected identifier, got {:?}", other))),
        }
    }

    fn error(&self, msg: &str) -> CypherError {
        CypherError::Parse(format!("at position {}: {}", self.pos, msg))
    }

    // ---- MATCH ----

    fn parse_match(&mut self) -> Result<Statement, CypherError> {
        self.expect(Token::Match)?;
        let pattern = self.parse_pattern()?;

        let where_clause = if matches!(self.peek(), Token::Where) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };

        let mut return_clause = None;
        let mut delete_clause = None;

        match self.peek() {
            Token::Return => {
                return_clause = Some(self.parse_return()?);
            }
            Token::Delete => {
                delete_clause = Some(self.parse_delete()?);
            }
            _ => {}
        }

        Ok(Statement::Match(MatchClause {
            pattern,
            where_clause,
            return_clause,
            delete_clause,
        }))
    }

    // ---- CREATE ----

    fn parse_create(&mut self) -> Result<Statement, CypherError> {
        self.expect(Token::Create)?;
        let pattern = self.parse_pattern()?;
        Ok(Statement::Create(CreateClause { pattern }))
    }

    // ---- MERGE ----

    fn parse_merge(&mut self) -> Result<Statement, CypherError> {
        self.expect(Token::Merge)?;
        let node = self.parse_node_pattern()?;
        Ok(Statement::Merge(MergeClause { pattern: node }))
    }

    // ---- RETURN ----

    fn parse_return(&mut self) -> Result<ReturnClause, CypherError> {
        self.expect(Token::Return)?;
        let mut items = vec![self.parse_return_item()?];
        while matches!(self.peek(), Token::Comma) {
            self.advance();
            items.push(self.parse_return_item()?);
        }
        let limit = if matches!(self.peek(), Token::Limit) {
            self.advance();
            match self.advance() {
                Token::IntLit(n) => Some(n as u64),
                other => return Err(self.error(&format!("expected integer after LIMIT, got {:?}", other))),
            }
        } else {
            None
        };
        Ok(ReturnClause { items, limit })
    }

    fn parse_return_item(&mut self) -> Result<ReturnItem, CypherError> {
        let item = if matches!(self.peek(), Token::Count) {
            self.advance();
            self.expect(Token::LParen)?;
            let inner = self.parse_return_item_atom()?;
            self.expect(Token::RParen)?;
            ReturnItem::FunctionCall("count".into(), Box::new(inner))
        } else {
            self.parse_return_item_atom()?
        };

        if matches!(self.peek(), Token::As) {
            self.advance();
            let alias = self.expect_ident()?;
            Ok(ReturnItem::Alias(Box::new(item), alias))
        } else {
            Ok(item)
        }
    }

    fn parse_return_item_atom(&mut self) -> Result<ReturnItem, CypherError> {
        let name = self.expect_ident()?;
        if matches!(self.peek(), Token::Dot) {
            self.advance();
            let prop = self.expect_ident()?;
            Ok(ReturnItem::Property(name, prop))
        } else {
            Ok(ReturnItem::Variable(name))
        }
    }

    // ---- DELETE ----

    fn parse_delete(&mut self) -> Result<Vec<String>, CypherError> {
        self.expect(Token::Delete)?;
        let mut names = vec![self.expect_ident()?];
        while matches!(self.peek(), Token::Comma) {
            self.advance();
            names.push(self.expect_ident()?);
        }
        Ok(names)
    }

    // ---- Pattern ----

    fn parse_pattern(&mut self) -> Result<Pattern, CypherError> {
        let mut elements = Vec::new();
        elements.push(PatternElement::Node(self.parse_node_pattern()?));

        loop {
            match self.peek() {
                Token::Dash => {
                    let edge = self.parse_edge_pattern()?;
                    elements.push(PatternElement::Edge(edge));
                    elements.push(PatternElement::Node(self.parse_node_pattern()?));
                }
                Token::Lt => {
                    // <-[...]- left-directed edge
                    let edge = self.parse_edge_pattern()?;
                    elements.push(PatternElement::Edge(edge));
                    elements.push(PatternElement::Node(self.parse_node_pattern()?));
                }
                _ => break,
            }
        }

        Ok(Pattern { elements })
    }

    fn parse_node_pattern(&mut self) -> Result<NodePattern, CypherError> {
        self.expect(Token::LParen)?;

        let mut variable = None;
        let mut label = None;
        let mut properties = HashMap::new();

        // Optional variable name
        if matches!(self.peek(), Token::Ident(_)) {
            variable = Some(self.expect_ident()?);
        }

        // Optional label
        if matches!(self.peek(), Token::Colon) {
            self.advance();
            label = Some(self.expect_ident()?);
        }

        // Optional properties
        if matches!(self.peek(), Token::LBrace) {
            properties = self.parse_properties()?;
        }

        self.expect(Token::RParen)?;

        Ok(NodePattern {
            variable,
            label,
            properties,
        })
    }

    fn parse_edge_pattern(&mut self) -> Result<EdgePattern, CypherError> {
        // Possible forms:
        //   -[r:TYPE]->   right
        //   <-[r:TYPE]-   left
        //   -[r:TYPE]-    both
        //   -[:TYPE]->    right, no variable
        //   -[]->         right, no type, no variable
        //   -->           right, shorthand (no brackets)
        //   -[]-          both

        let left_arrow = if matches!(self.peek(), Token::Lt) {
            self.advance(); // consume <
            true
        } else {
            false
        };

        self.expect(Token::Dash)?;

        let mut variable = None;
        let mut edge_type = None;

        if matches!(self.peek(), Token::LBracket) {
            self.advance();

            if matches!(self.peek(), Token::Ident(_)) {
                variable = Some(self.expect_ident()?);
            }

            if matches!(self.peek(), Token::Colon) {
                self.advance();
                edge_type = Some(self.expect_ident()?);
            }

            self.expect(Token::RBracket)?;
        }

        let direction = if left_arrow {
            self.expect(Token::Dash)?;
            EdgeDirection::Left
        } else if matches!(self.peek(), Token::Arrow) {
            self.advance(); // consume ->
            EdgeDirection::Right
        } else if matches!(self.peek(), Token::Dash) {
            self.advance();
            EdgeDirection::Both
        } else {
            // just - after ] with no further direction marker = undirected
            EdgeDirection::Both
        };

        Ok(EdgePattern {
            variable,
            edge_type,
            direction,
        })
    }

    fn parse_properties(&mut self) -> Result<HashMap<String, Expr>, CypherError> {
        self.expect(Token::LBrace)?;
        let mut props = HashMap::new();

        if !matches!(self.peek(), Token::RBrace) {
            let key = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let value = self.parse_expr()?;
            props.insert(key, value);

            while matches!(self.peek(), Token::Comma) {
                self.advance();
                let key = self.expect_ident()?;
                self.expect(Token::Colon)?;
                let value = self.parse_expr()?;
                props.insert(key, value);
            }
        }

        self.expect(Token::RBrace)?;
        Ok(props)
    }

    // ---- Expressions ----

    fn parse_expr(&mut self) -> Result<Expr, CypherError> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> Result<Expr, CypherError> {
        let mut left = self.parse_and_expr()?;
        while matches!(self.peek(), Token::Or) {
            self.advance();
            let right = self.parse_and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> Result<Expr, CypherError> {
        let mut left = self.parse_not_expr()?;
        while matches!(self.peek(), Token::And) {
            self.advance();
            let right = self.parse_not_expr()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not_expr(&mut self) -> Result<Expr, CypherError> {
        if matches!(self.peek(), Token::Not) {
            self.advance();
            let inner = self.parse_not_expr()?;
            Ok(Expr::Not(Box::new(inner)))
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr, CypherError> {
        let left = self.parse_atom()?;
        let op = match self.peek() {
            Token::Eq => Some(CmpOp::Eq),
            Token::Neq => Some(CmpOp::Neq),
            Token::Lt => Some(CmpOp::Lt),
            Token::Gt => Some(CmpOp::Gt),
            Token::Lte => Some(CmpOp::Lte),
            Token::Gte => Some(CmpOp::Gte),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let right = self.parse_atom()?;
            Ok(Expr::Comparison(Box::new(left), op, Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, CypherError> {
        match self.peek().clone() {
            Token::IntLit(n) => {
                let n = n;
                self.advance();
                Ok(Expr::Literal(LiteralValue::Int(n)))
            }
            Token::FloatLit(f) => {
                let f = f;
                self.advance();
                Ok(Expr::Literal(LiteralValue::Float(f)))
            }
            Token::StringLit(s) => {
                let s = s.clone();
                self.advance();
                Ok(Expr::Literal(LiteralValue::String(s)))
            }
            Token::True => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Bool(false)))
            }
            Token::Null => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Null))
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                if matches!(self.peek(), Token::Dot) {
                    self.advance();
                    let prop = self.expect_ident()?;
                    Ok(Expr::Property(name, prop))
                } else {
                    // Bare identifier — not supported as an expression atom in this subset.
                    // Treat as an error for now; property access requires dot notation.
                    Err(self.error(&format!("bare identifier '{}' not allowed in expressions; use property access like {}.prop", name, name)))
                }
            }
            Token::LParen => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(inner)
            }
            _ => Err(self.error(&format!("unexpected token in expression: {:?}", self.peek()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(input: &str) -> Statement {
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse().unwrap()
    }

    #[test]
    fn parse_match_return_node() {
        let stmt = parse("MATCH (n:Person) RETURN n");
        match stmt {
            Statement::Match(m) => {
                assert_eq!(m.pattern.elements.len(), 1);
                match &m.pattern.elements[0] {
                    PatternElement::Node(np) => {
                        assert_eq!(np.variable.as_deref(), Some("n"));
                        assert_eq!(np.label.as_deref(), Some("Person"));
                    }
                    _ => panic!("expected node pattern"),
                }
                let ret = m.return_clause.unwrap();
                assert_eq!(ret.items.len(), 1);
                assert_eq!(ret.items[0], ReturnItem::Variable("n".into()));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_match_edge_pattern() {
        let stmt = parse("MATCH (n)-[:KNOWS]->(m) RETURN n, m");
        match stmt {
            Statement::Match(m) => {
                assert_eq!(m.pattern.elements.len(), 3); // node, edge, node
                match &m.pattern.elements[1] {
                    PatternElement::Edge(ep) => {
                        assert_eq!(ep.edge_type.as_deref(), Some("KNOWS"));
                        assert_eq!(ep.direction, EdgeDirection::Right);
                    }
                    _ => panic!("expected edge pattern"),
                }
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_create_node_with_props() {
        let stmt = parse("CREATE (n:Person {name: 'Alice', age: 30})");
        match stmt {
            Statement::Create(c) => {
                assert_eq!(c.pattern.elements.len(), 1);
                match &c.pattern.elements[0] {
                    PatternElement::Node(np) => {
                        assert_eq!(np.variable.as_deref(), Some("n"));
                        assert_eq!(np.label.as_deref(), Some("Person"));
                        assert_eq!(np.properties.len(), 2);
                        assert_eq!(
                            np.properties.get("name"),
                            Some(&Expr::Literal(LiteralValue::String("Alice".into())))
                        );
                        assert_eq!(
                            np.properties.get("age"),
                            Some(&Expr::Literal(LiteralValue::Int(30)))
                        );
                    }
                    _ => panic!("expected node pattern"),
                }
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn parse_create_edge_pattern() {
        let stmt = parse("CREATE (n:Person {name: 'Alice'})-[:KNOWS]->(m:Person {name: 'Bob'})");
        match stmt {
            Statement::Create(c) => {
                assert_eq!(c.pattern.elements.len(), 3);
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn parse_match_with_where() {
        let stmt = parse("MATCH (n:Person) WHERE n.age > 30 RETURN n.name");
        match stmt {
            Statement::Match(m) => {
                assert!(m.where_clause.is_some());
                match m.where_clause.unwrap() {
                    Expr::Comparison(left, CmpOp::Gt, right) => {
                        assert_eq!(*left, Expr::Property("n".into(), "age".into()));
                        assert_eq!(*right, Expr::Literal(LiteralValue::Int(30)));
                    }
                    _ => panic!("expected comparison"),
                }
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_match_with_limit() {
        let stmt = parse("MATCH (n:Person) RETURN n LIMIT 10");
        match stmt {
            Statement::Match(m) => {
                let ret = m.return_clause.unwrap();
                assert_eq!(ret.limit, Some(10));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_match_delete() {
        let stmt = parse("MATCH (n:Person {name: 'Alice'}) DELETE n");
        match stmt {
            Statement::Match(m) => {
                assert_eq!(m.delete_clause, Some(vec!["n".into()]));
                assert!(m.return_clause.is_none());
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_merge() {
        let stmt = parse("MERGE (n:Person {name: 'Alice'})");
        match stmt {
            Statement::Merge(m) => {
                assert_eq!(m.pattern.label.as_deref(), Some("Person"));
                assert_eq!(m.pattern.variable.as_deref(), Some("n"));
            }
            _ => panic!("expected Merge"),
        }
    }

    #[test]
    fn parse_count_function() {
        let stmt = parse("MATCH (n:Person) RETURN count(n)");
        match stmt {
            Statement::Match(m) => {
                let ret = m.return_clause.unwrap();
                match &ret.items[0] {
                    ReturnItem::FunctionCall(name, inner) => {
                        assert_eq!(name, "count");
                        assert_eq!(**inner, ReturnItem::Variable("n".into()));
                    }
                    _ => panic!("expected function call"),
                }
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_error_on_invalid() {
        let mut lexer = Lexer::new("MATCH RETURN");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        assert!(parser.parse().is_err());
    }

    #[test]
    fn parse_property_return() {
        let stmt = parse("MATCH (n:Person) RETURN n.name, n.age");
        match stmt {
            Statement::Match(m) => {
                let ret = m.return_clause.unwrap();
                assert_eq!(ret.items.len(), 2);
                assert_eq!(ret.items[0], ReturnItem::Property("n".into(), "name".into()));
                assert_eq!(ret.items[1], ReturnItem::Property("n".into(), "age".into()));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_multi_hop() {
        let stmt = parse("MATCH (a)-[:X]->(b)-[:Y]->(c) RETURN a, b, c");
        match stmt {
            Statement::Match(m) => {
                // node, edge, node, edge, node = 5 elements
                assert_eq!(m.pattern.elements.len(), 5);
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_edge_variable() {
        let stmt = parse("MATCH (n)-[r:KNOWS]->(m) RETURN n, r, m");
        match stmt {
            Statement::Match(m) => {
                match &m.pattern.elements[1] {
                    PatternElement::Edge(ep) => {
                        assert_eq!(ep.variable.as_deref(), Some("r"));
                        assert_eq!(ep.edge_type.as_deref(), Some("KNOWS"));
                    }
                    _ => panic!("expected edge"),
                }
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn parse_where_and_or() {
        let stmt = parse("MATCH (n:Person) WHERE n.age > 20 AND n.age < 50 RETURN n");
        match stmt {
            Statement::Match(m) => {
                match m.where_clause.unwrap() {
                    Expr::And(_, _) => {}
                    other => panic!("expected And, got {:?}", other),
                }
            }
            _ => panic!("expected Match"),
        }
    }
}
