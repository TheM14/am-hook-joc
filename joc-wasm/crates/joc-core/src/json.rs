//! Small JSON reader for the `.jochrtf` metadata member.
//!
//! Only top-level scalar members are needed; nested values are kept as their raw
//! source text.  The parser is strict about syntax so a corrupted cache fails
//! loudly instead of silently losing a field.

#[derive(Clone, Debug)]
pub enum Value {
    String(String),
    Integer(i64),
    Number(f64),
    Bool(bool),
    Null,
    Raw(String),
}

pub struct Member {
    pub key: String,
    pub value: Value,
}

pub fn parse_object(text: &str) -> Result<Vec<Member>, String> {
    let mut parser = Parser { bytes: text.as_bytes(), pos: 0 };
    parser.skip_ws();
    if !parser.eat(b'{') {
        return Err("metadata is not a JSON object".into());
    }
    let mut members = Vec::new();
    parser.skip_ws();
    if parser.eat(b'}') {
        return Ok(members);
    }
    loop {
        parser.skip_ws();
        let key = parser.string()?;
        parser.skip_ws();
        if !parser.eat(b':') {
            return Err("expected ':' after a JSON key".into());
        }
        parser.skip_ws();
        let start = parser.pos;
        let value = parser.value()?;
        members.push(Member {
            key,
            value: match value {
                Value::Raw(_) => Value::Raw(text[start..parser.pos].to_string()),
                other => other,
            },
        });
        parser.skip_ws();
        if parser.eat(b',') {
            continue;
        }
        if parser.eat(b'}') {
            return Ok(members);
        }
        return Err("expected ',' or '}' in a JSON object".into());
    }
}

pub fn find<'a>(members: &'a [Member], key: &str) -> Option<&'a Member> {
    members.iter().find(|member| member.key == key)
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Integer(value) => Some(*value as f64),
            Value::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(value) => Some(*value),
            Value::Number(value) if value.fract() == 0.0 => Some(*value as i64),
            _ => None,
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.pos < self.bytes.len() && self.bytes[self.pos] == byte {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if !self.eat(b'"') {
            return Err("expected a JSON string".into());
        }
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            self.pos += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape = *self.bytes.get(self.pos).ok_or("truncated JSON escape")?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.pos..self.pos + 4)
                                .ok_or("truncated JSON unicode escape")?;
                            let text = core::str::from_utf8(hex).map_err(|_| "bad unicode escape")?;
                            let code = u32::from_str_radix(text, 16)
                                .map_err(|_| "bad unicode escape")?;
                            self.pos += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => return Err(format!("unknown JSON escape \\{}", other as char)),
                    }
                }
                _ => {
                    // ASCII or a UTF-8 continuation byte: collect raw bytes and let
                    // String validation happen at the end.
                    let start = self.pos - 1;
                    let mut end = self.pos;
                    while end < self.bytes.len()
                        && self.bytes[end] != b'"'
                        && self.bytes[end] != b'\\'
                    {
                        end += 1;
                    }
                    out.push_str(
                        core::str::from_utf8(&self.bytes[start..end])
                            .map_err(|_| "invalid UTF-8 in JSON string")?,
                    );
                    self.pos = end;
                }
            }
        }
        Err("unterminated JSON string".into())
    }

    fn value(&mut self) -> Result<Value, String> {
        let byte = *self.bytes.get(self.pos).ok_or("truncated JSON value")?;
        match byte {
            b'"' => Ok(Value::String(self.string()?)),
            b'{' | b'[' => {
                self.skip_container()?;
                Ok(Value::Raw(String::new()))
            }
            b't' => {
                self.literal("true")?;
                Ok(Value::Bool(true))
            }
            b'f' => {
                self.literal("false")?;
                Ok(Value::Bool(false))
            }
            b'n' => {
                self.literal("null")?;
                Ok(Value::Null)
            }
            _ => self.number(),
        }
    }

    fn literal(&mut self, text: &str) -> Result<(), String> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(())
        } else {
            Err(format!("expected JSON literal {text}"))
        }
    }

    fn skip_container(&mut self) -> Result<(), String> {
        let open = self.bytes[self.pos];
        let close = if open == b'{' { b'}' } else { b']' };
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            match byte {
                b'"' => {
                    self.string()?;
                    continue;
                }
                _ if byte == open => depth += 1,
                _ if byte == close => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        Err("unterminated JSON container".into())
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.pos < self.bytes.len() && (self.bytes[self.pos] == b'-' || self.bytes[self.pos] == b'+') {
            self.pos += 1;
        }
        let mut seen_dot = false;
        let mut seen_exp = false;
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            if byte.is_ascii_digit() {
                self.pos += 1;
            } else if byte == b'.' && !seen_dot && !seen_exp {
                seen_dot = true;
                self.pos += 1;
            } else if (byte == b'e' || byte == b'E') && !seen_exp {
                seen_exp = true;
                self.pos += 1;
                if self.pos < self.bytes.len()
                    && (self.bytes[self.pos] == b'-' || self.bytes[self.pos] == b'+')
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        let text = core::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| "bad number")?;
        if text.is_empty() {
            return Err("expected a JSON value".into());
        }
        if !seen_dot && !seen_exp {
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Value::Integer(value));
            }
        }
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| format!("bad JSON number {text}"))
    }
}
