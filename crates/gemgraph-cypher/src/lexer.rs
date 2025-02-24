use crate::CypherError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Match,
    Return,
    Where,
    Create,
    Delete,
    Merge,
    Limit,
    And,
    Or,
    Not,
    Count,
    As,
    True,
    False,
    Null,

    // Symbols
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Arrow, // ->
    Dash,  // -

    // Comparison
    Gt,
    Lt,
    Gte,
    Lte,
    Eq,
    Neq, // <>

    // Literals
    Ident(String),
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),

    // Meta
    Eof,
}

pub struct Lexer {
    input: Vec<char>,
    pos: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Lexer {
            input: input.chars().collect(),
            pos: 0,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, CypherError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            if tok == Token::Eof {
                tokens.push(Token::Eof);
                break;
            }
            tokens.push(tok);
        }
        Ok(tokens)
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.input.get(self.pos).copied()?;
        self.pos += 1;
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, CypherError> {
        self.skip_whitespace();

        let ch = match self.peek() {
            Some(c) => c,
            None => return Ok(Token::Eof),
        };

        match ch {
            '(' => { self.advance(); Ok(Token::LParen) }
            ')' => { self.advance(); Ok(Token::RParen) }
            '[' => { self.advance(); Ok(Token::LBracket) }
            ']' => { self.advance(); Ok(Token::RBracket) }
            '{' => { self.advance(); Ok(Token::LBrace) }
            '}' => { self.advance(); Ok(Token::RBrace) }
            ':' => { self.advance(); Ok(Token::Colon) }
            ',' => { self.advance(); Ok(Token::Comma) }
            '.' => { self.advance(); Ok(Token::Dot) }
            '=' => { self.advance(); Ok(Token::Eq) }
            '-' => {
                self.advance();
                if self.peek() == Some('>') {
                    self.advance();
                    Ok(Token::Arrow)
                } else {
                    Ok(Token::Dash)
                }
            }
            '>' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(Token::Gte)
                } else {
                    Ok(Token::Gt)
                }
            }
            '<' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(Token::Lte)
                } else if self.peek() == Some('>') {
                    self.advance();
                    Ok(Token::Neq)
                } else {
                    Ok(Token::Lt)
                }
            }
            '\'' => self.read_string(),
            '"' => self.read_double_string(),
            c if c.is_ascii_digit() => self.read_number(),
            c if c.is_alphabetic() || c == '_' => self.read_ident_or_keyword(),
            _ => Err(CypherError::Lex(format!("unexpected character: '{}'", ch))),
        }
    }

    fn read_string(&mut self) -> Result<Token, CypherError> {
        self.advance(); // consume opening quote
        let mut s = String::new();
        loop {
            match self.advance() {
                Some('\'') => return Ok(Token::StringLit(s)),
                Some('\\') => {
                    match self.advance() {
                        Some(c) => s.push(c),
                        None => return Err(CypherError::Lex("unterminated string".into())),
                    }
                }
                Some(c) => s.push(c),
                None => return Err(CypherError::Lex("unterminated string".into())),
            }
        }
    }

    fn read_double_string(&mut self) -> Result<Token, CypherError> {
        self.advance(); // consume opening quote
        let mut s = String::new();
        loop {
            match self.advance() {
                Some('"') => return Ok(Token::StringLit(s)),
                Some('\\') => {
                    match self.advance() {
                        Some(c) => s.push(c),
                        None => return Err(CypherError::Lex("unterminated string".into())),
                    }
                }
                Some(c) => s.push(c),
                None => return Err(CypherError::Lex("unterminated string".into())),
            }
        }
    }

    fn read_number(&mut self) -> Result<Token, CypherError> {
        let mut s = String::new();
        let mut is_float = false;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                s.push(ch);
                self.advance();
            } else if ch == '.' && !is_float {
                is_float = true;
                s.push(ch);
                self.advance();
            } else {
                break;
            }
        }
        if is_float {
            let val: f64 = s.parse().map_err(|e: std::num::ParseFloatError| {
                CypherError::Lex(e.to_string())
            })?;
            Ok(Token::FloatLit(val))
        } else {
            let val: i64 = s.parse().map_err(|e: std::num::ParseIntError| {
                CypherError::Lex(e.to_string())
            })?;
            Ok(Token::IntLit(val))
        }
    }

    fn read_ident_or_keyword(&mut self) -> Result<Token, CypherError> {
        let mut s = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                s.push(ch);
                self.advance();
            } else {
                break;
            }
        }
        let upper = s.to_uppercase();
        let tok = match upper.as_str() {
            "MATCH" => Token::Match,
            "RETURN" => Token::Return,
            "WHERE" => Token::Where,
            "CREATE" => Token::Create,
            "DELETE" => Token::Delete,
            "MERGE" => Token::Merge,
            "LIMIT" => Token::Limit,
            "AND" => Token::And,
            "OR" => Token::Or,
            "NOT" => Token::Not,
            "COUNT" => Token::Count,
            "AS" => Token::As,
            "TRUE" => Token::True,
            "FALSE" => Token::False,
            "NULL" => Token::Null,
            _ => Token::Ident(s),
        };
        Ok(tok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_match_return() {
        let mut lexer = Lexer::new("MATCH (n:Person) RETURN n");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![
            Token::Match,
            Token::LParen,
            Token::Ident("n".into()),
            Token::Colon,
            Token::Ident("Person".into()),
            Token::RParen,
            Token::Return,
            Token::Ident("n".into()),
            Token::Eof,
        ]);
    }

    #[test]
    fn lex_string_literal() {
        let mut lexer = Lexer::new("'hello world'");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![Token::StringLit("hello world".into()), Token::Eof]);
    }

    #[test]
    fn lex_numbers() {
        let mut lexer = Lexer::new("42 3.14");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![Token::IntLit(42), Token::FloatLit(3.14), Token::Eof]);
    }

    #[test]
    fn lex_case_insensitive_keywords() {
        let mut lexer = Lexer::new("match RETURN where");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![Token::Match, Token::Return, Token::Where, Token::Eof]);
    }

    #[test]
    fn lex_comparison_operators() {
        let mut lexer = Lexer::new("> < >= <= = <>");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![
            Token::Gt, Token::Lt, Token::Gte, Token::Lte, Token::Eq, Token::Neq, Token::Eof,
        ]);
    }

    #[test]
    fn lex_edge_pattern() {
        let mut lexer = Lexer::new("-[:KNOWS]->");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![
            Token::Dash,
            Token::LBracket,
            Token::Colon,
            Token::Ident("KNOWS".into()),
            Token::RBracket,
            Token::Arrow,
            Token::Eof,
        ]);
    }

    #[test]
    fn lex_properties() {
        let mut lexer = Lexer::new("{name: 'Alice', age: 30}");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![
            Token::LBrace,
            Token::Ident("name".into()),
            Token::Colon,
            Token::StringLit("Alice".into()),
            Token::Comma,
            Token::Ident("age".into()),
            Token::Colon,
            Token::IntLit(30),
            Token::RBrace,
            Token::Eof,
        ]);
    }

    #[test]
    fn lex_boolean_and_null() {
        let mut lexer = Lexer::new("true false null");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens, vec![Token::True, Token::False, Token::Null, Token::Eof]);
    }
}
