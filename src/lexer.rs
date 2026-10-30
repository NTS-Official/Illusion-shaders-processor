//! Minimal GLSL lexer used by the dialect normaliser.
//!
//! Whitespace and comments are kept as tokens so a declaration can be
//! rewritten without reformatting the rest of the file.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Space,
    Comment,
    Ident,
    Number,
    Symbol,
    Directive,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub line: u32,
}

impl Token {
    pub fn new(kind: Kind, text: &str, line: u32) -> Self {
        Self {
            kind,
            text: text.to_owned(),
            line,
        }
    }

    pub fn is_trivia(&self) -> bool {
        matches!(self.kind, Kind::Space | Kind::Comment)
    }
}

pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    let mut line_has_code = false;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\n' => {
                tokens.push(Token::new(Kind::Space, "\n", line));
                line += 1;
                line_has_code = false;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => {
                let start = i;
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r') {
                    i += 1;
                }
                tokens.push(Token::new(Kind::Space, &src[start..i], line));
            }
            b'#' if !line_has_code => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                tokens.push(Token::new(Kind::Directive, &src[start..i], line));
                line_has_code = true;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                tokens.push(Token::new(Kind::Comment, &src[start..i], line));
                line_has_code = true;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        i += 2;
                        break;
                    }
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                tokens.push(Token::new(Kind::Comment, &src[start..i], line));
                line_has_code = true;
            }
            _ if is_ident_start(b) => {
                let start = i;
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                tokens.push(Token::new(Kind::Ident, &src[start..i], line));
                line_has_code = true;
            }
            _ if b.is_ascii_digit()
                || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) =>
            {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                    i += 1;
                }
                tokens.push(Token::new(Kind::Number, &src[start..i], line));
                line_has_code = true;
            }
            _ => {
                let start = i;
                i += if b < 0x80 {
                    1
                } else {
                    // Advance to the next UTF-8 boundary without going backwards.
                    let mut step = 1;
                    while i + step < bytes.len() && (bytes[i + step] & 0xC0) == 0x80 {
                        step += 1;
                    }
                    step
                };
                tokens.push(Token::new(Kind::Symbol, &src[start..i], line));
                line_has_code = true;
            }
        }
    }

    tokens
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub fn render(tokens: &[Token]) -> String {
    let mut out = String::with_capacity(tokens.iter().map(|t| t.text.len()).sum());
    for token in tokens {
        out.push_str(&token.text);
    }
    out
}

pub fn next_significant(tokens: &[Token], mut index: usize) -> Option<usize> {
    while index < tokens.len() {
        if !tokens[index].is_trivia() {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub fn prev_significant(tokens: &[Token], mut index: usize) -> Option<usize> {
    loop {
        index = index.checked_sub(1)?;
        if !tokens[index].is_trivia() {
            return Some(index);
        }
    }
}

pub fn is_symbol(token: &Token, text: &str) -> bool {
    token.kind == Kind::Symbol && token.text == text
}

pub fn is_ident(token: &Token, text: &str) -> bool {
    token.kind == Kind::Ident && token.text == text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_trivia_and_tracks_lines() {
        let tokens = tokenize("vec4 a;\n// c\nfloat b;\n");
        let text = render(&tokens);
        assert_eq!(text, "vec4 a;\n// c\nfloat b;\n");
        assert_eq!(tokens.iter().find(|t| t.text == "float").unwrap().line, 3);
    }

    #[test]
    fn directive_is_one_token() {
        let tokens = tokenize("#version 330\n");
        assert_eq!(tokens[0].kind, Kind::Directive);
        assert_eq!(tokens[0].text, "#version 330");
    }

    #[test]
    fn hash_after_code_is_not_a_directive() {
        let tokens = tokenize("float a = 1; #define X 2\n");
        assert!(tokens.iter().all(|t| t.kind != Kind::Directive));
    }
}
