//! Minimal RFC 8259 JSON: a value tree, a strict parser and a serializer whose
//! output is byte-identical to JavaScript's `JSON.stringify`.
//!
//! Why byte-identical: job records under `.agy-plugin/jobs/` were written by
//! the old Node runtime with `JSON.stringify(record, null, 2)`. Old and new
//! binaries must be able to read and rewrite each other's files, and diffs of
//! those files should stay quiet across the migration.
//!
//! Why strict and depth-limited: the parser also reads agy's
//! `--output-format json` result, which is output of an external program and
//! therefore treated as hostile. Every failure is a `ParseError`, never a
//! panic, and nesting is capped so a `[[[[...` bomb cannot overflow the stack.

use std::collections::HashMap;
use std::fmt;

/// Maximum nesting of arrays/objects accepted by `parse`. Real job records
/// and agy results nest 2-3 levels; the cap only exists to bound recursion.
const MAX_DEPTH: usize = 128;

/// Objects at or below this size look up duplicate keys by linear scan; above
/// it the parser builds a hash index so a hostile object with many keys costs
/// O(n) instead of O(n^2).
const LINEAR_SCAN_LIMIT: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Map),
}

/// Insertion-ordered object. Order matters because JS preserves insertion
/// order for string keys and the job files must serialize in the same order.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Map(Vec<(String, Value)>);

impl Map {
    pub(crate) fn new() -> Self {
        Map(Vec::new())
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Replaces an existing key in place (keeping its position, as assigning
    /// to an existing property does in JS), otherwise appends.
    pub(crate) fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        let value = value.into();
        match self.get_mut(&key) {
            Some(slot) => *slot = value,
            None => self.0.push((key, value)),
        }
    }

    /// Removes a key, keeping the relative order of the remaining entries.
    #[cfg_attr(not(test), allow(dead_code))] // general JSON API; exercised by tests
    pub(crate) fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(idx).1)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    #[cfg_attr(not(test), allow(dead_code))] // general JSON API; exercised by tests
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg_attr(not(test), allow(dead_code))] // general JSON API; exercised by tests
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

impl Value {
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Only integral numbers that fit in i64 exactly; 1.5 or 1e19 give None
    /// rather than a silently truncated/saturated value.
    #[cfg_attr(not(test), allow(dead_code))] // general JSON API; exercised by tests
    pub(crate) fn as_i64(&self) -> Option<i64> {
        let n = self.as_f64()?;
        // -2^63 is exactly representable; 2^63 is the first value out of range.
        if n.is_finite()
            && n.fract() == 0.0
            && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n)
        {
            Some(n as i64)
        } else {
            None
        }
    }

    pub(crate) fn as_u64(&self) -> Option<u64> {
        let n = self.as_f64()?;
        if n.is_finite() && n.fract() == 0.0 && (0.0..18_446_744_073_709_551_616.0).contains(&n) {
            Some(n as u64)
        } else {
            None
        }
    }

    #[cfg_attr(not(test), allow(dead_code))] // general JSON API; exercised by tests
    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))] // general JSON API; exercised by tests
    pub(crate) fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub(crate) fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    pub(crate) fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Object member lookup; None for non-objects (like optional chaining).
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

// Integers beyond 2^53 lose precision, exactly as they would in JS.
impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n as f64)
    }
}

impl From<u64> for Value {
    fn from(n: u64) -> Self {
        Value::Number(n as f64)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Number(f64::from(n))
    }
}

impl From<u32> for Value {
    fn from(n: u32) -> Self {
        Value::Number(f64::from(n))
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Value::Number(n as f64)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Self {
        o.map_or(Value::Null, Into::into)
    }
}

impl From<Vec<Value>> for Value {
    fn from(a: Vec<Value>) -> Self {
        Value::Array(a)
    }
}

impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::Object(m)
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// `offset` is a byte offset into the text given to `parse` (a leading BOM
/// counts as its 3 bytes), so it can be used to slice the original input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) offset: usize,
    pub(crate) message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid JSON at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parses one JSON document (RFC 8259, plus an optional leading UTF-8 BOM).
///
/// Lossy on purpose in one place: a lone or mismatched UTF-16 surrogate in a
/// `\uXXXX` escape becomes U+FFFD, because a Rust `String` cannot hold it.
/// JS would keep the lone surrogate and `JSON.stringify` would re-escape it,
/// so such a string does not round-trip byte-for-byte; nothing we persist
/// contains lone surrogates. Numbers that overflow f64 (e.g. `1e400`) become
/// infinity, as in JS, and therefore serialize back as `null`.
pub(crate) fn parse(text: &str) -> Result<Value, ParseError> {
    // Windows editors (Notepad and friends) prefix saved files with a BOM.
    let start = if text.starts_with('\u{FEFF}') { 3 } else { 0 };
    let mut p = Parser {
        text,
        bytes: text.as_bytes(),
        pos: start,
    };
    p.skip_ws();
    if p.pos >= p.bytes.len() {
        return Err(p.err("empty input"));
    }
    let value = p.parse_value(0)?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        return Err(p.err("unexpected trailing characters after JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, message: &str) -> ParseError {
        self.err_at(self.pos, message)
    }

    fn err_at(&self, offset: usize, message: &str) -> ParseError {
        ParseError {
            offset,
            message: message.to_owned(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        // RFC 8259 whitespace only: no form feed, vertical tab, NBSP or BOM.
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect_literal(&mut self, word: &str, value: Value) -> Result<Value, ParseError> {
        let end = self.pos.saturating_add(word.len());
        if self.bytes.get(self.pos..end) == Some(word.as_bytes()) {
            self.pos = end;
            Ok(value)
        } else {
            Err(self.err(&format!("invalid literal, expected '{word}'")))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, ParseError> {
        match self.peek() {
            None => Err(self.err("unexpected end of input, expected a value")),
            Some(b'{') => self.parse_object(depth + 1),
            Some(b'[') => self.parse_array(depth + 1),
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b't') => self.expect_literal("true", Value::Bool(true)),
            Some(b'f') => self.expect_literal("false", Value::Bool(false)),
            Some(b'n') => self.expect_literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.err("unexpected character, expected a value")),
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth > MAX_DEPTH {
            Err(self.err(&format!("nesting deeper than {MAX_DEPTH} levels")))
        } else {
            Ok(())
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.check_depth(depth)?;
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                None => return Err(self.err("unterminated array")),
                Some(_) => return Err(self.err("expected ',' or ']' in array")),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.check_depth(depth)?;
        self.pos += 1; // '{'
        let mut entries: Vec<(String, Value)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(Map(entries)));
        }
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'"') => {}
                None => return Err(self.err("unterminated object")),
                Some(_) => return Err(self.err("expected string key in object")),
            }
            let key = self.parse_string()?;
            self.skip_ws();
            match self.peek() {
                Some(b':') => self.pos += 1,
                None => return Err(self.err("unterminated object")),
                Some(_) => return Err(self.err("expected ':' after object key")),
            }
            self.skip_ws();
            let value = self.parse_value(depth)?;

            // Duplicate keys: last value wins, first position is kept. That is
            // what JSON.parse does (a repeated assignment to the same property).
            let existing = if entries.len() <= LINEAR_SCAN_LIMIT {
                entries.iter().position(|(k, _)| *k == key)
            } else {
                if index.is_empty() {
                    index = entries
                        .iter()
                        .enumerate()
                        .map(|(i, (k, _))| (k.clone(), i))
                        .collect();
                }
                index.get(&key).copied()
            };
            match existing.and_then(|i| entries.get_mut(i)) {
                Some(slot) => slot.1 = value,
                None => {
                    if !index.is_empty() {
                        index.insert(key.clone(), entries.len());
                    }
                    entries.push((key, value));
                }
            }

            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(Map(entries)));
                }
                None => return Err(self.err("unterminated object")),
                Some(_) => return Err(self.err("expected ',' or '}' in object")),
            }
        }
    }

    fn eat_digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.err("leading zeros are not allowed in numbers"));
                }
            }
            Some(b'1'..=b'9') => {
                self.eat_digits();
            }
            _ => return Err(self.err("invalid number, expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.eat_digits() == 0 {
                return Err(self.err("invalid number, expected a digit after '.'"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.eat_digits() == 0 {
                return Err(self.err("invalid number, expected a digit in exponent"));
            }
        }
        // The grammar above is a subset of what Rust's f64 parser accepts, and
        // Rust's parser is correctly rounded, so 2^53-range integers are exact.
        self.text
            .get(start..self.pos)
            .and_then(|s| s.parse::<f64>().ok())
            .map(Value::Number)
            .ok_or_else(|| self.err_at(start, "invalid number"))
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let end = at.checked_add(4)?;
        let digits = self.bytes.get(at..end)?;
        let mut code = 0u32;
        for &d in digits {
            code = code * 16 + char::from(d).to_digit(16)?;
        }
        Some(code)
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.pos += 1; // '"'
        let mut out = String::new();
        loop {
            let run_start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // The run stops only at ASCII bytes or end of input, so both ends
            // are char boundaries; `get` keeps this panic-free regardless.
            match self.text.get(run_start..self.pos) {
                Some(run) => out.push_str(run),
                None => return Err(self.err_at(run_start, "invalid UTF-8 boundary in string")),
            }
            match self.peek() {
                None => return Err(self.err_at(open, "unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => self.parse_escape(&mut out)?,
                Some(_) => return Err(self.err("unescaped control character in string")),
            }
        }
    }

    /// Called with `pos` on the backslash.
    fn parse_escape(&mut self, out: &mut String) -> Result<(), ParseError> {
        let esc = self.bytes.get(self.pos + 1).copied();
        let simple = match esc {
            Some(b'"') => Some('"'),
            Some(b'\\') => Some('\\'),
            Some(b'/') => Some('/'),
            Some(b'b') => Some('\u{8}'),
            Some(b'f') => Some('\u{c}'),
            Some(b'n') => Some('\n'),
            Some(b'r') => Some('\r'),
            Some(b't') => Some('\t'),
            Some(b'u') => None,
            None => return Err(self.err("unterminated escape sequence")),
            Some(_) => return Err(self.err("invalid escape sequence")),
        };
        if let Some(c) = simple {
            out.push(c);
            self.pos += 2;
            return Ok(());
        }
        let Some(code) = self.hex4(self.pos + 2) else {
            return Err(self.err("invalid \\u escape, expected 4 hex digits"));
        };
        self.pos += 6;
        let ch = match code {
            0xD800..=0xDBFF => {
                // A high surrogate only means something when a low one follows
                // as the very next escape. Otherwise it is lone: U+FFFD, and
                // whatever follows is parsed on its own (and may still error).
                let low = if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                    self.hex4(self.pos + 2)
                        .filter(|lo| (0xDC00..=0xDFFF).contains(lo))
                } else {
                    None
                };
                match low {
                    Some(lo) => {
                        self.pos += 6;
                        char::from_u32(0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00))
                            .unwrap_or('\u{FFFD}')
                    }
                    None => '\u{FFFD}',
                }
            }
            0xDC00..=0xDFFF => '\u{FFFD}',
            _ => char::from_u32(code).unwrap_or('\u{FFFD}'),
        };
        out.push(ch);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Serializer
// ---------------------------------------------------------------------------

/// Same bytes as `JSON.stringify(v)`.
pub(crate) fn to_string_compact(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, false, 0);
    out
}

/// Same bytes as `JSON.stringify(v, null, 2)`; no trailing newline (the old
/// code appended "\n" itself when writing files, and so must callers).
pub(crate) fn to_string_pretty(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, true, 0);
    out
}

fn newline_indent(out: &mut String, level: usize) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, v: &Value, pretty: bool, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => write_number(out, *n),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    newline_indent(out, level + 1);
                }
                write_value(out, item, pretty, level + 1);
            }
            if pretty {
                newline_indent(out, level);
            }
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    newline_indent(out, level + 1);
                }
                write_string(out, key);
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                write_value(out, item, pretty, level + 1);
            }
            if pretty {
                newline_indent(out, level);
            }
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // JS uses lowercase hex here. U+2028/U+2029 and DEL stay raw, as
            // JSON.stringify leaves them.
            c if u32::from(c) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Splits Rust's `{:e}` output ("1.2345e21") into ("12345", 21).
fn split_sci(sci: &str) -> (String, i64) {
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let digits = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exp.parse().unwrap_or(0))
}

/// Rust's shortest-digits algorithm and V8's agree except when the exact
/// binary value lies precisely halfway between the two shortest decimal
/// candidates (e.g. 801130354720945.25 -> "...945.2" or "...945.3"). Rust
/// rounds such ties up; V8 (bignum-dtoa) rounds them to the even digit. Found
/// by diffing 200k random doubles against node: ~0.04% differed, all ties.
/// So detect the exact tie from the full decimal expansion and pick the even
/// candidate, keeping it only if it still round-trips to `x`.
fn js_shortest_digits(x: f64) -> (String, i64) {
    let (digits, exp) = split_sci(&format!("{x:e}"));
    // Every finite f64 has an exact decimal expansion of at most 767
    // significant digits, so 1100 digits after the point is exact.
    let (exact, exact_exp) = split_sci(&format!("{x:.1100e}"));
    let k = digits.len();
    if exact_exp != exp {
        return (digits, exp);
    }
    let (Some(lower), Some(tail)) = (exact.get(..k), exact.get(k..)) else {
        return (digits, exp);
    };
    let is_tie = tail.starts_with('5') && tail.bytes().skip(1).all(|b| b == b'0');
    if !is_tie {
        return (digits, exp);
    }
    let lower_is_even = lower
        .bytes()
        .last()
        .is_some_and(|b| b.wrapping_sub(b'0') % 2 == 0);
    let even = if lower_is_even {
        Some(lower.to_owned())
    } else {
        increment_digits(lower)
    };
    let Some(even) = even else {
        return (digits, exp);
    };
    if even == digits || even.is_empty() {
        return (digits, exp);
    }
    let (first, rest) = even.split_at(1);
    if format!("{first}.{rest}0e{exp}").parse::<f64>() == Ok(x) {
        (even, exp)
    } else {
        (digits, exp)
    }
}

/// Decimal digit string + 1, or None when it would carry into a new digit
/// (that changes the exponent; the caller then keeps Rust's answer).
fn increment_digits(digits: &str) -> Option<String> {
    let mut bytes = digits.as_bytes().to_vec();
    for b in bytes.iter_mut().rev() {
        if *b == b'9' {
            *b = b'0';
        } else {
            *b += 1;
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

/// ECMAScript Number::toString(10), which JSON.stringify uses for finite
/// numbers: shortest round-trip digits (see `js_shortest_digits`) laid out by
/// the spec's fixed/exponential rules.
fn write_number(out: &mut String, x: f64) {
    if !x.is_finite() {
        // JSON has no NaN/Infinity; JSON.stringify writes null.
        out.push_str("null");
        return;
    }
    if x == 0.0 {
        // Covers -0, which JS prints as "0".
        out.push('0');
        return;
    }
    let (digits, exp) = js_shortest_digits(x.abs());
    // Value = 0.DIGITS * 10^n, in the spec's terms: k digits, exponent n.
    let k = digits.len() as i64;
    let n = exp + 1;

    if x < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        let (int_part, frac_part) = digits.split_at(n as usize);
        out.push_str(int_part);
        out.push('.');
        out.push_str(frac_part);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let (first, rest) = digits.split_at(1.min(digits.len()));
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let e = n - 1;
        out.push('e');
        out.push(if e >= 0 { '+' } else { '-' });
        out.push_str(&e.abs().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB_RECORD: &str = r#"{
  "id": "agy-2b067725",
  "kind": "review",
  "task": "review: Be brief. List the bugs in calc.js.",
  "model": null,
  "workspaceRoot": "/private/var/folders/jg/x/T/tmp.SN7LQWIwMi/demo",
  "createdAt": "2026-05-31T15:02:38.473Z",
  "startedAt": "2026-05-31T15:02:38.485Z",
  "completedAt": "2026-05-31T15:02:46.851Z",
  "status": "completed",
  "exitCode": 0,
  "pid": null,
  "logFile": ".agy-plugin/logs/agy-2b067725.log",
  "args": [],
  "background": false,
  "meta": {
    "prompt": "Review the following diff.\n— dash \"quoted\"",
    "agyBin": "agy",
    "executionRoot": null,
    "stageDir": "/tmp/agy-review-BTZ7RN"
  },
  "lastUpdatedAt": "2026-05-31T15:02:46.852Z"
}"#;

    const AGY_RESULT: &str = r#"{"conversation_id":"0795225f-aaaa","status":"SUCCESS","response":"OK\n","duration_seconds":3.000251,"num_turns":1,"usage":{"input_tokens":10137,"output_tokens":1},"denied_actions":[{"action":"write_file","display_name":"Edit"}]}"#;

    fn num(x: f64) -> String {
        to_string_compact(&Value::Number(x))
    }

    fn s(text: &str) -> String {
        match parse(text) {
            Ok(Value::String(s)) => s,
            other => panic!("expected string from {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn job_record_round_trips_byte_for_byte() {
        let v = parse(JOB_RECORD).unwrap();
        assert_eq!(to_string_pretty(&v), JOB_RECORD);
        // The file on disk has a trailing newline appended by the writer.
        let with_nl = format!("{JOB_RECORD}\n");
        assert_eq!(parse(&with_nl).unwrap(), v);

        assert_eq!(v.get("id").and_then(Value::as_str), Some("agy-2b067725"));
        assert!(v.get("model").unwrap().is_null());
        assert_eq!(v.get("exitCode").and_then(Value::as_i64), Some(0));
        assert_eq!(v.get("background").and_then(Value::as_bool), Some(false));
        assert_eq!(
            v.get("args").and_then(Value::as_array).map(<[Value]>::len),
            Some(0)
        );
        let prompt = v
            .get("meta")
            .and_then(|m| m.get("prompt"))
            .and_then(Value::as_str);
        assert_eq!(
            prompt,
            Some("Review the following diff.\n\u{2014} dash \"quoted\"")
        );
        let keys: Vec<&str> = v.as_object().unwrap().iter().map(|(k, _)| k).collect();
        assert_eq!(keys.first(), Some(&"id"));
        assert_eq!(keys.last(), Some(&"lastUpdatedAt"));
        assert_eq!(keys.len(), 16);
    }

    #[test]
    fn agy_result_parses_and_round_trips_compact() {
        let v = parse(AGY_RESULT).unwrap();
        assert_eq!(v.get("status").and_then(Value::as_str), Some("SUCCESS"));
        assert_eq!(v.get("response").and_then(Value::as_str), Some("OK\n"));
        assert_eq!(
            v.get("duration_seconds").and_then(Value::as_f64),
            Some(3.000251)
        );
        assert_eq!(v.get("duration_seconds").and_then(Value::as_i64), None);
        let usage = v.get("usage").unwrap();
        assert_eq!(
            usage.get("input_tokens").and_then(Value::as_u64),
            Some(10137)
        );
        let denied = v.get("denied_actions").and_then(Value::as_array).unwrap();
        assert_eq!(
            denied[0].get("action").and_then(Value::as_str),
            Some("write_file")
        );
        assert_eq!(to_string_compact(&v), AGY_RESULT);
    }

    #[test]
    fn bom_is_skipped_only_at_start() {
        assert_eq!(
            parse("\u{FEFF}{\"a\":1}").unwrap().get("a"),
            Some(&Value::Number(1.0))
        );
        assert_eq!(parse("\u{FEFF}  [ ]\n").unwrap(), Value::Array(vec![]));
        assert!(parse("\u{FEFF}").is_err());
        assert!(parse("\u{FEFF}\u{FEFF}1").is_err());
        assert!(parse(" \u{FEFF}1").is_err());
        assert!(parse("[\u{FEFF}1]").is_err());
        // Offsets are into the original text, BOM included.
        assert_eq!(parse("\u{FEFF}x").unwrap_err().offset, 3);
    }

    #[test]
    fn surrogate_pairs_and_lone_surrogates() {
        assert_eq!(s(r#""\ud83d\ude00""#), "\u{1F600}");
        assert_eq!(s(r#""\uD83D\uDE00""#), "\u{1F600}");
        assert_eq!(s("\"\u{1F600}\""), "\u{1F600}");
        assert_eq!(s(r#""\ud800x""#), "\u{FFFD}x");
        assert_eq!(s(r#""\udc00""#), "\u{FFFD}");
        assert_eq!(s(r#""\ud800""#), "\u{FFFD}");
        assert_eq!(s(r#""\ud800\ud83d\ude00""#), "\u{FFFD}\u{1F600}");
        assert_eq!(s(r#""\ude00\ud83d""#), "\u{FFFD}\u{FFFD}");
        assert_eq!(s(r#""\ud800\n""#), "\u{FFFD}\n");
        assert_eq!(s(r#""\ud800\u0041""#), "\u{FFFD}A");
        // A malformed escape after a high surrogate is still an error.
        assert!(parse(r#""\ud800\u12""#).is_err());
        assert!(parse(r#""\ud800\uZZZZ""#).is_err());
        // Emoji serializes raw, like JS.
        assert_eq!(
            to_string_compact(&Value::from("\u{1F600}")),
            "\"\u{1F600}\""
        );
    }

    #[test]
    fn escapes_parse() {
        assert_eq!(s(r#""\"\\\/\b\f\n\r\t""#), "\"\\/\u{8}\u{c}\n\r\t");
        assert_eq!(s(r#""\u0041\u00e9\u00E9\u0000""#), "A\u{e9}\u{e9}\u{0}");
        assert_eq!(s(r#""a\u2028b""#), "a\u{2028}b");
        assert_eq!(s("\"\u{7f}\""), "\u{7f}");
        for bad in [
            r#""\x""#,
            r#""\U0041""#,
            r#""\u004""#,
            r#""\u00g0""#,
            r#""\'""#,
            "\"\\",
            "\"\\u",
        ] {
            assert!(parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn escapes_serialize_like_js() {
        // Expected from node: JSON.stringify("x\u0001\u001f\u007f\u2028\u2029\b\f\n\r\t\"\\/é😀")
        let input = "x\u{1}\u{1f}\u{7f}\u{2028}\u{2029}\u{8}\u{c}\n\r\t\"\\/\u{e9}\u{1F600}";
        let expected =
            "\"x\\u0001\\u001f\u{7f}\u{2028}\u{2029}\\b\\f\\n\\r\\t\\\"\\\\/\u{e9}\u{1F600}\"";
        assert_eq!(to_string_compact(&Value::from(input)), expected);
        assert_eq!(
            to_string_compact(&Value::from("\u{0}\u{b}\u{1b}")),
            "\"\\u0000\\u000b\\u001b\""
        );
        assert_eq!(s(expected), input);
    }

    #[test]
    fn rejects_malformed_input() {
        let cases = [
            "",
            " ",
            "\n\t\r ",
            "01",
            "-01",
            "00",
            "+1",
            ".5",
            "1.",
            "1.e5",
            "-",
            "--1",
            "1e",
            "1e+",
            "1E-",
            "NaN",
            "Infinity",
            "-Infinity",
            "nan",
            "0x10",
            "1_000",
            "tru",
            "true1",
            "nul",
            "nulll",
            "False",
            "[1,]",
            "[,1]",
            "[1 2]",
            "[",
            "]",
            "{",
            "}",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\" 1}",
            "{a:1}",
            "{'a':1}",
            "{\"a\":1,}",
            "{,}",
            "{\"a\":1 \"b\":2}",
            "'x'",
            "\"abc",
            "\"a\nb\"",
            "\"a\tb\"",
            "\"\u{1}\"",
            "\"\u{1f}\"",
            "1 2",
            "{} {}",
            "[]x",
            "null,",
            "\u{b}1",
            "\u{c}1",
            "\u{a0}1",
            "1\u{a0}",
            "[1]\u{0}",
            "/*c*/1",
            "1//c",
            "[\"a\",\"b\"",
            "{\"a\":[}",
        ];
        for c in cases {
            assert!(parse(c).is_err(), "should reject {c:?}");
        }
    }

    #[test]
    fn error_offsets_and_messages() {
        let e = parse("[1,]").unwrap_err();
        assert_eq!(e.offset, 3);
        assert_eq!(parse("01").unwrap_err().offset, 1);
        assert!(parse("01").unwrap_err().message.contains("leading zero"));
        assert_eq!(parse("\"a\u{1}\"").unwrap_err().offset, 2);
        assert_eq!(parse("  \"abc").unwrap_err().offset, 2);
        assert_eq!(parse("1 2").unwrap_err().offset, 2);
        assert!(parse("").unwrap_err().message.contains("empty"));
        assert!(parse("  ").unwrap_err().message.contains("empty"));
        let shown = parse("[1,]").unwrap_err().to_string();
        assert!(shown.contains("byte 3"), "{shown}");
    }

    #[test]
    fn whitespace_accepted() {
        let v = parse(" \t\r\n{ \"a\" \n:\t[ 1 , 2 ] \r} \n").unwrap();
        assert_eq!(to_string_compact(&v), r#"{"a":[1,2]}"#);
    }

    #[test]
    fn depth_limit() {
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(parse(&ok).is_ok());
        let too_deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert!(parse(&too_deep).unwrap_err().message.contains("nesting"));

        let obj_ok = format!("{}1{}", "{\"a\":".repeat(MAX_DEPTH), "}".repeat(MAX_DEPTH));
        assert!(parse(&obj_ok).is_ok());
        let obj_deep = format!(
            "{}1{}",
            "{\"a\":".repeat(MAX_DEPTH + 1),
            "}".repeat(MAX_DEPTH + 1)
        );
        assert!(parse(&obj_deep).is_err());

        // A bomb far past the limit must error, not overflow the stack.
        assert!(parse(&"[".repeat(1_000_000)).is_err());
        assert!(parse(&"{\"a\":[".repeat(200_000)).is_err());
    }

    #[test]
    fn duplicate_keys_last_wins_first_position() {
        let v = parse(r#"{"a":1,"b":2,"a":3}"#).unwrap();
        assert_eq!(to_string_compact(&v), r#"{"a":3,"b":2}"#);

        // Large object: exercises the hash-index path.
        let mut text = String::from("{");
        for i in 0..40 {
            text.push_str(&format!("\"k{i}\":{i},"));
        }
        text.push_str("\"k3\":\"x\",\"k39\":true,\"new\":null,\"k0\":[]}");
        let v = parse(&text).unwrap();
        let m = v.as_object().unwrap();
        assert_eq!(m.len(), 41);
        let keys: Vec<&str> = m.iter().map(|(k, _)| k).collect();
        assert_eq!(keys[0], "k0");
        assert_eq!(keys[3], "k3");
        assert_eq!(keys[40], "new");
        assert_eq!(m.get("k0"), Some(&Value::Array(vec![])));
        assert_eq!(m.get("k3"), Some(&Value::from("x")));
        assert_eq!(m.get("k39"), Some(&Value::Bool(true)));
        assert_eq!(m.get("k20"), Some(&Value::Number(20.0)));
    }

    #[test]
    fn pretty_matches_js() {
        // node -e 'console.log(JSON.stringify({a:[1,{b:[]},{}],c:"x",d:null,e:true},null,2))'
        let v = parse(r#"{"a":[1,{"b":[]},{}],"c":"x","d":null,"e":true}"#).unwrap();
        let expected = "{\n  \"a\": [\n    1,\n    {\n      \"b\": []\n    },\n    {}\n  ],\n  \"c\": \"x\",\n  \"d\": null,\n  \"e\": true\n}";
        assert_eq!(to_string_pretty(&v), expected);

        // node: JSON.stringify([[],[[]],{"":{}}],null,2)
        let v = parse(r#"[[],[[]],{"":{}}]"#).unwrap();
        let expected = "[\n  [],\n  [\n    []\n  ],\n  {\n    \"\": {}\n  }\n]";
        assert_eq!(to_string_pretty(&v), expected);

        assert_eq!(to_string_pretty(&Value::Array(vec![])), "[]");
        assert_eq!(to_string_pretty(&Value::Object(Map::new())), "{}");
        assert_eq!(to_string_pretty(&Value::from("a\nb")), "\"a\\nb\"");
        assert_eq!(to_string_pretty(&Value::Null), "null");
    }

    #[test]
    fn compact_matches_js() {
        let v = parse(
            r#" { "a" : [ 1 , { "b" : [ ] } , { } ] , "c" : "x" , "d" : null , "e" : true } "#,
        )
        .unwrap();
        assert_eq!(
            to_string_compact(&v),
            r#"{"a":[1,{"b":[]},{}],"c":"x","d":null,"e":true}"#
        );
        assert_eq!(to_string_compact(&Value::Bool(false)), "false");
    }

    #[test]
    fn numbers_serialize_like_js() {
        // Expected strings produced by node's JSON.stringify.
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (9007199254740992.0, "9007199254740992"),
            (9007199254740994.0, "9007199254740994"),
            (0.1, "0.1"),
            (1e-7, "1e-7"),
            (1e-6, "0.000001"),
            (0.000001234, "0.000001234"),
            (-1.5e300, "-1.5e+300"),
            (1.5, "1.5"),
            (-1.0, "-1"),
            (42.0, "42"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (123.456, "123.456"),
            (1e100, "1e+100"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-1e-7, "-1e-7"),
            (1.2345e21, "1.2345e+21"),
            (100.0, "100"),
            (1e-10, "1e-10"),
            (18446744073709551616.0, "18446744073709552000"),
            (-9223372036854775808.0, "-9223372036854776000"),
            (3.000251, "3.000251"),
            (f64::NAN, "null"),
            (f64::INFINITY, "null"),
            (f64::NEG_INFINITY, "null"),
        ];
        for &(x, want) in cases {
            assert_eq!(num(x), want, "for {x:?}");
        }
    }

    #[test]
    fn number_ties_round_half_even_like_v8() {
        // Exact binary values halfway between two shortest decimals. Rust's
        // `{:e}` rounds these up; node prints the even digit. Expected
        // strings come from node's JSON.stringify.
        let cases: &[(u64, &str)] = &[
            (0x4306_c4fd_91b2_058a, "801130354720945.2"),
            (0x4289_44ae_34f4_8d40, "3472846397073.6562"),
            (0xc303_e1b7_152e_0932, "-699525126013222.2"),
            (0x424e_5059_4f9a_5200, "260394098484.64062"),
        ];
        for &(bits, want) in cases {
            assert_eq!(num(f64::from_bits(bits)), want, "for bits {bits:x}");
        }
        // Written as sums because the exact literal trips clippy's
        // excessive_precision; at this magnitude ulp is 0.125, so both exact.
        let base = 801_130_354_720_945.0_f64;
        assert_eq!(num(base + 0.75), "801130354720945.8");
        assert_eq!(num(base + 0.25), "801130354720945.2");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(2.5), "2.5");
        assert_eq!(num(1e23), "1e+23");
        assert_eq!(num(9.5e-7), "9.5e-7");
        assert_eq!(increment_digits("129"), Some("130".to_owned()));
        assert_eq!(increment_digits("99"), None);
    }

    #[test]
    fn numbers_parse() {
        let n = |t: &str| parse(t).unwrap().as_f64().unwrap();
        assert_eq!(n("0"), 0.0);
        assert!(n("-0").is_sign_negative());
        assert_eq!(num(n("-0")), "0");
        assert_eq!(n("1E5"), 1e5);
        assert_eq!(n("1e+2"), 100.0);
        assert_eq!(n("1.5e-3"), 1.5e-3);
        assert_eq!(n("-1.5e300"), -1.5e300);
        assert_eq!(n("9007199254740992"), 9007199254740992.0);
        assert_eq!(parse("9007199254740992").unwrap().as_i64(), Some(1 << 53));
        assert_eq!(num(n("9007199254740992")), "9007199254740992");
        // 2^53 + 1 rounds to even, as in JS.
        assert_eq!(n("9007199254740993"), 9007199254740992.0);
        assert_eq!(n("0.1"), 0.1);
        assert!(n("1e400").is_infinite());
        assert_eq!(n("1e-400"), 0.0);
        assert_eq!(n(&format!("1{}", "0".repeat(400))), f64::INFINITY);
    }

    #[test]
    fn integers_round_trip_up_to_2_pow_53() {
        for i in [
            0i64,
            1,
            -1,
            255,
            1_000_000,
            (1 << 31) - 1,
            1 << 40,
            (1 << 53) - 1,
            1 << 53,
            -(1 << 53),
        ] {
            let text = to_string_compact(&Value::from(i));
            assert_eq!(text, i.to_string());
            assert_eq!(parse(&text).unwrap().as_i64(), Some(i));
        }
    }

    #[test]
    fn value_accessors() {
        assert_eq!(Value::Number(1.5).as_i64(), None);
        assert_eq!(Value::Number(-1.0).as_i64(), Some(-1));
        assert_eq!(Value::Number(-1.0).as_u64(), None);
        assert_eq!(Value::Number(-0.0).as_u64(), Some(0));
        assert_eq!(Value::Number(1e19).as_i64(), None);
        assert_eq!(
            Value::Number(1e19).as_u64(),
            Some(10_000_000_000_000_000_000)
        );
        assert_eq!(Value::Number(9223372036854775808.0).as_i64(), None);
        assert_eq!(
            Value::Number(-9223372036854775808.0).as_i64(),
            Some(i64::MIN)
        );
        assert_eq!(Value::Number(18446744073709551616.0).as_u64(), None);
        assert_eq!(Value::Number(f64::NAN).as_i64(), None);
        assert_eq!(Value::Number(f64::INFINITY).as_u64(), None);
        assert_eq!(Value::from("1").as_i64(), None);
        assert_eq!(Value::from("x").as_str(), Some("x"));
        assert_eq!(Value::Null.as_str(), None);
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Null.as_bool(), None);
        assert!(Value::Null.is_null());
        assert!(!Value::Bool(false).is_null());
        assert_eq!(Value::from("x").get("a"), None);
        assert_eq!(Value::Array(vec![]).get("a"), None);
        assert_eq!(Value::Null.as_array(), None);
        assert_eq!(Value::Null.as_object(), None);

        let mut v = Value::Array(vec![Value::Null]);
        v.as_array_mut().unwrap().push(Value::from(1));
        assert_eq!(to_string_compact(&v), "[null,1]");
        let mut o = Value::Object(Map::new());
        o.as_object_mut().unwrap().set("k", "v");
        assert_eq!(o.get("k").and_then(Value::as_str), Some("v"));
        assert!(Value::Null.as_object_mut().is_none());
    }

    #[test]
    fn from_impls() {
        assert_eq!(Value::from("a"), Value::String("a".into()));
        assert_eq!(Value::from(String::from("a")), Value::String("a".into()));
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(-3i64), Value::Number(-3.0));
        assert_eq!(Value::from(3u64), Value::Number(3.0));
        assert_eq!(Value::from(-3i32), Value::Number(-3.0));
        assert_eq!(Value::from(3u32), Value::Number(3.0));
        assert_eq!(Value::from(3usize), Value::Number(3.0));
        assert_eq!(Value::from(2.5f64), Value::Number(2.5));
        assert_eq!(Value::from(None::<&str>), Value::Null);
        assert_eq!(Value::from(Some("x")), Value::from("x"));
        assert_eq!(Value::from(Some(7i64)), Value::Number(7.0));
        assert_eq!(
            Value::from(vec![Value::Null]),
            Value::Array(vec![Value::Null])
        );
        assert_eq!(Value::from(Map::new()), Value::Object(Map::new()));
    }

    #[test]
    fn map_api() {
        let mut m = Map::new();
        assert!(m.is_empty());
        m.set("a", 1i64);
        m.set("b", "two");
        m.set(String::from("c"), Value::Null);
        m.set("a", true); // replaces in place
        assert_eq!(m.len(), 3);
        assert!(m.contains_key("b"));
        assert!(!m.contains_key("z"));
        assert_eq!(
            to_string_compact(&Value::from(m.clone())),
            r#"{"a":true,"b":"two","c":null}"#
        );

        if let Some(v) = m.get_mut("b") {
            *v = Value::from(2i64);
        }
        assert_eq!(m.get("b"), Some(&Value::Number(2.0)));
        assert!(m.get_mut("zz").is_none());

        assert_eq!(m.remove("a"), Some(Value::Bool(true)));
        assert_eq!(m.remove("a"), None);
        m.set("a", 0i64); // re-added at the end
        let keys: Vec<&str> = m.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["b", "c", "a"]);
        assert_eq!(Map::default(), Map::new());
    }

    /// xorshift64*: deterministic, std-only randomness for the fuzz test.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// If a mutated document parses, it must also survive a serialize/parse
    /// round trip unchanged (modulo NaN, which JSON cannot express).
    fn check_no_panic_and_stable(text: &str) {
        if let Ok(v) = parse(text) {
            let compact = to_string_compact(&v);
            let pretty = to_string_pretty(&v);
            let back = parse(&compact).expect("serializer output must parse");
            assert_eq!(to_string_compact(&back), compact);
            assert_eq!(parse(&pretty).map(|p| to_string_compact(&p)), Ok(compact));
        }
    }

    #[test]
    fn fuzz_truncations_and_mutations_never_panic() {
        let seeds = [
            JOB_RECORD,
            AGY_RESULT,
            r#"{"s":"\ud83d\ude00\u0041\n\"\\","n":[-0,1e-7,1.5E+3,123456789012345678901234],"e":{},"a":[[[]]],"t":true,"f":false,"z":null}"#,
            "\u{FEFF}[\"\u{e9}\u{1F600}\u{2028}\",{\"k\":\"\\ud800\"}]",
        ];
        // Characters that are likely to produce interesting near-valid JSON.
        let alphabet: Vec<char> =
            "{}[]\":,\\/ \n\t\r0123456789-+.eEtrufalsn\u{0}\u{1f}\u{e9}\u{FEFF}\u{1F600}"
                .chars()
                .collect();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut runs = 0usize;

        for seed in seeds {
            assert!(parse(seed).is_ok(), "seed must be valid: {seed}");
            let chars: Vec<char> = seed.chars().collect();
            // Every prefix and every suffix.
            for i in 0..=chars.len() {
                let prefix: String = chars[..i].iter().collect();
                let suffix: String = chars[i..].iter().collect();
                check_no_panic_and_stable(&prefix);
                check_no_panic_and_stable(&suffix);
                runs += 2;
            }
            // Random point mutations: replace, insert, delete, duplicate a span.
            for _ in 0..3000 {
                let mut m = chars.clone();
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(m.len() + 1);
                    match rng.below(4) {
                        0 if at < m.len() => m[at] = alphabet[rng.below(alphabet.len())],
                        1 => m.insert(at, alphabet[rng.below(alphabet.len())]),
                        2 if at < m.len() => {
                            m.remove(at);
                        }
                        _ => {
                            let end = (at + rng.below(16)).min(m.len());
                            let span: Vec<char> = m[at..end].to_vec();
                            let to = rng.below(m.len() + 1);
                            m.splice(to..to, span);
                        }
                    }
                }
                let text: String = m.into_iter().collect();
                check_no_panic_and_stable(&text);
                runs += 1;
            }
        }
        // Pure random garbage from the alphabet.
        for _ in 0..3000 {
            let len = rng.below(64);
            let text: String = (0..len)
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            check_no_panic_and_stable(&text);
            runs += 1;
        }
        assert!(runs > 15_000);
    }
}
