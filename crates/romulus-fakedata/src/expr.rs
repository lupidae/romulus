//! Expressions as Postgres prints them: CHECKs (`pg_get_constraintdef`),
//! defaults and generation expressions, partition bounds and index
//! definitions.
//!
//! Not `pg_query`: that is C, and this crate builds for wasm. What Postgres
//! prints is regular (parenthesised, constants cast, `IN` as `= ANY
//! (ARRAY[…])`, `BETWEEN` as two comparisons), so a small Pratt parser reads
//! it. Anything it does not know becomes [`Expr::Unknown`] (or the whole
//! parse fails): the caller then leaves that expression to Postgres.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// An identifier, folded to lower case when unquoted.
    Ident {
        name: String,
        quoted: bool,
    },
    Str(String),
    Num(String),
    Op(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Semicolon,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident { name, .. } => write!(f, "{name}"),
            Tok::Str(s) => write!(f, "'{s}'"),
            Tok::Num(n) | Tok::Op(n) => write!(f, "{n}"),
            Tok::LParen => f.write_str("("),
            Tok::RParen => f.write_str(")"),
            Tok::LBracket => f.write_str("["),
            Tok::RBracket => f.write_str("]"),
            Tok::Comma => f.write_str(","),
            Tok::Dot => f.write_str("."),
            Tok::Semicolon => f.write_str(";"),
        }
    }
}

const OP_CHARS: &str = "+-*/<>=~!@#%^&|`?";

/// Splits `text` into tokens; `None` on what Postgres would not print
/// (an unterminated string, a stray character).
#[allow(clippy::too_many_lines)]
pub fn lex(text: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // An escape string, E'…' (Postgres prints one for backslashes).
        if (c == 'E' || c == 'e') && chars.get(i + 1) == Some(&'\'') {
            let (s, next) = string(&chars, i + 1, true)?;
            out.push(Tok::Str(s));
            i = next;
            continue;
        }
        if c == '\'' {
            let (s, next) = string(&chars, i, false)?;
            out.push(Tok::Str(s));
            i = next;
            continue;
        }
        if c == '"' {
            let mut s = String::new();
            let mut j = i + 1;
            loop {
                match chars.get(j) {
                    None => return None,
                    Some('"') if chars.get(j + 1) == Some(&'"') => {
                        s.push('"');
                        j += 2;
                    }
                    Some('"') => break,
                    Some(&ch) => {
                        s.push(ch);
                        j += 1;
                    }
                }
            }
            out.push(Tok::Ident {
                name: s,
                quoted: true,
            });
            i = j + 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
                // `1..2` is not a number Postgres prints; stop at a second dot.
                if chars[j] == '.' && chars[i..j].contains(&'.') {
                    break;
                }
                j += 1;
            }
            if j < chars.len() && (chars[j] == 'e' || chars[j] == 'E') {
                let mut k = j + 1;
                if k < chars.len() && (chars[k] == '+' || chars[k] == '-') {
                    k += 1;
                }
                if k < chars.len() && chars[k].is_ascii_digit() {
                    while k < chars.len() && chars[k].is_ascii_digit() {
                        k += 1;
                    }
                    j = k;
                }
            }
            out.push(Tok::Num(chars[i..j].iter().collect()));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '$')
            {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            out.push(Tok::Ident {
                name: word.to_lowercase(),
                quoted: false,
            });
            i = j;
            continue;
        }
        match c {
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            '[' => out.push(Tok::LBracket),
            ']' => out.push(Tok::RBracket),
            ',' => out.push(Tok::Comma),
            ';' => out.push(Tok::Semicolon),
            '.' => out.push(Tok::Dot),
            ':' if chars.get(i + 1) == Some(&':') => {
                out.push(Tok::Op("::".to_owned()));
                i += 2;
                continue;
            }
            _ if OP_CHARS.contains(c) => {
                let mut j = i;
                while j < chars.len() && OP_CHARS.contains(chars[j]) {
                    // A comment never appears in what Postgres prints, but
                    // `--` would start one: stop before it.
                    if j > i && chars[j] == '-' && chars[j - 1] == '-' {
                        break;
                    }
                    j += 1;
                }
                let mut op: String = chars[i..j].iter().collect();
                // Postgres's rule: a multi-char operator ends with + or -
                // only if it holds one of ~ ! @ # % ^ & | ` ?
                while op.len() > 1
                    && (op.ends_with('+') || op.ends_with('-'))
                    && !op.chars().any(|ch| "~!@#%^&|`?".contains(ch))
                {
                    op.pop();
                }
                i += op.chars().count();
                out.push(Tok::Op(op));
                continue;
            }
            _ => return None,
        }
        i += 1;
    }
    Some(out)
}

fn string(chars: &[char], start: usize, escapes: bool) -> Option<(String, usize)> {
    let mut s = String::new();
    let mut j = start + 1;
    loop {
        match chars.get(j) {
            None => return None,
            Some('\'') if chars.get(j + 1) == Some(&'\'') => {
                s.push('\'');
                j += 2;
            }
            Some('\'') => return Some((s, j + 1)),
            Some('\\') if escapes => {
                let next = *chars.get(j + 1)?;
                s.push(match next {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    other => other,
                });
                j += 2;
            }
            Some(&ch) => {
                s.push(ch);
                j += 1;
            }
        }
    }
}

/// A literal constant.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Str(String),
    Num(String),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A column (the last part of a dotted name).
    Col(String),
    /// `VALUE` in a domain's CHECK.
    Value,
    Lit(Lit),
    /// `expr::type`, the type as written (`character varying`, `public.citext`).
    Cast(Box<Expr>, String),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    /// Any other binary operator, `OPERATOR(schema.op)` read as `op`.
    Bin(String, Box<Expr>, Box<Expr>),
    /// `left op ANY (right)` (`all` for `ALL`).
    Quantified {
        op: String,
        all: bool,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `expr [NOT] IN (list)`.
    In(Box<Expr>, Vec<Expr>, bool),
    /// `expr [NOT] BETWEEN low AND high`.
    Between(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    /// `expr IS [NOT] NULL`.
    IsNull(Box<Expr>, bool),
    /// `expr IS [NOT] TRUE|FALSE`: (value, negated).
    IsBool(Box<Expr>, bool, bool),
    /// A call, the name lower-cased and qualified as written.
    Func(String, Vec<Expr>),
    /// `ARRAY[…]`.
    Array(Vec<Expr>),
    /// `(a, b)`, or `ROW(a, b)`.
    Row(Vec<Expr>),
    /// `CURRENT_DATE`, `CURRENT_TIMESTAMP`, `LOCALTIMESTAMP`…
    Keyword(String),
    /// Something this parser does not model (CASE, subscripts…).
    Unknown,
}

impl Expr {
    /// Columns the expression refers to, in order of appearance.
    pub fn columns(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.walk(&mut |e| {
            if let Expr::Col(name) = e
                && !out.contains(name)
            {
                out.push(name.clone());
            }
        });
        out
    }

    /// Whether anything in it is [`Expr::Unknown`].
    pub fn has_unknown(&self) -> bool {
        let mut unknown = false;
        self.walk(&mut |e| unknown |= matches!(e, Expr::Unknown));
        unknown
    }

    /// Function names it calls.
    pub fn calls(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.walk(&mut |e| {
            if let Expr::Func(name, _) = e {
                out.push(name.clone());
            }
        });
        out
    }

    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match self {
            Expr::Cast(e, _)
            | Expr::Not(e)
            | Expr::Neg(e)
            | Expr::IsNull(e, _)
            | Expr::IsBool(e, _, _) => e.walk(f),
            Expr::And(list) | Expr::Or(list) | Expr::Array(list) | Expr::Row(list) => {
                for e in list {
                    e.walk(f);
                }
            }
            Expr::Func(_, args) => {
                for e in args {
                    e.walk(f);
                }
            }
            Expr::Bin(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            Expr::Quantified { left, right, .. } => {
                left.walk(f);
                right.walk(f);
            }
            Expr::In(e, list, _) => {
                e.walk(f);
                for x in list {
                    x.walk(f);
                }
            }
            Expr::Between(e, lo, hi, _) => {
                e.walk(f);
                lo.walk(f);
                hi.walk(f);
            }
            Expr::Col(_) | Expr::Value | Expr::Lit(_) | Expr::Keyword(_) | Expr::Unknown => {}
        }
    }

    /// Top-level conjuncts (`a AND (b AND c)` → `[a, b, c]`).
    pub fn conjuncts(&self) -> Vec<&Expr> {
        match self {
            Expr::And(list) => list.iter().flat_map(Expr::conjuncts).collect(),
            other => vec![other],
        }
    }

    /// Without casts around it.
    pub fn uncast(&self) -> &Expr {
        match self {
            Expr::Cast(e, _) => e.uncast(),
            other => other,
        }
    }
}

/// Parses one expression; `None` when it is not one this parser reads.
pub fn parse(text: &str) -> Option<Expr> {
    let toks = lex(text)?;
    let mut p = Parser::new(toks);
    let e = p.expr(0)?;
    (p.pos == p.toks.len()).then_some(e)
}

/// The expression of a `CHECK (…)` definition (`NOT VALID` and
/// `NO INHERIT` allowed after it); `domain` for a domain's, where `VALUE`
/// is the value checked.
pub fn parse_check(definition: &str, domain: bool) -> Option<Expr> {
    let toks = lex(definition)?;
    let mut p = Parser::new(toks);
    p.domain = domain;
    if !p.eat_word("check") {
        return None;
    }
    p.expect(&Tok::LParen)?;
    let e = p.expr(0)?;
    p.expect(&Tok::RParen)?;
    while p.eat_word("not") || p.eat_word("valid") || p.eat_word("no") || p.eat_word("inherit") {}
    (p.pos == p.toks.len()).then_some(e)
}

pub struct Parser {
    pub toks: Vec<Tok>,
    pub pos: usize,
    /// In a domain's CHECK, `VALUE` is the value; elsewhere a column.
    pub domain: bool,
}

const MULTIWORD_TYPES: &[&str] = &[
    "character",
    "char",
    "double",
    "timestamp",
    "time",
    "bit",
    "interval",
    "national",
];

impl Parser {
    pub fn new(toks: Vec<Tok>) -> Self {
        Self {
            toks,
            pos: 0,
            domain: false,
        }
    }

    pub fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, offset: usize) -> Option<&Tok> {
        self.toks.get(self.pos + offset)
    }

    pub fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    pub fn expect(&mut self, tok: &Tok) -> Option<()> {
        (self.peek() == Some(tok)).then(|| self.pos += 1)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident { name, quoted: false }) if name == word)
    }

    fn is_word_at(&self, offset: usize, word: &str) -> bool {
        matches!(self.peek_at(offset), Some(Tok::Ident { name, quoted: false }) if name == word)
    }

    pub fn eat_word(&mut self, word: &str) -> bool {
        if self.is_word(word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// A type name after `::` (or in a column list), as written.
    pub fn type_name(&mut self) -> Option<String> {
        let mut out = String::new();
        let mut first_word = None;
        // Qualified name.
        loop {
            match self.next()? {
                Tok::Ident { name, quoted } => {
                    if first_word.is_none() && !quoted {
                        first_word = Some(name.clone());
                    }
                    if quoted {
                        out.push('"');
                        out.push_str(&name.replace('"', "\"\""));
                        out.push('"');
                    } else {
                        out.push_str(&name);
                    }
                }
                _ => return None,
            }
            if self.peek() == Some(&Tok::Dot) {
                self.pos += 1;
                out.push('.');
                first_word = None;
                continue;
            }
            break;
        }
        let multiword = first_word
            .as_deref()
            .is_some_and(|w| MULTIWORD_TYPES.contains(&w));
        loop {
            if self.peek() == Some(&Tok::LParen) && !out.ends_with(']') {
                // A type modifier: numbers and words only.
                let save = self.pos;
                self.pos += 1;
                let mut mods = Vec::new();
                let mut ok = false;
                while let Some(t) = self.next() {
                    match t {
                        Tok::RParen => {
                            ok = true;
                            break;
                        }
                        Tok::Comma => mods.push(",".to_owned()),
                        Tok::Num(n) => mods.push(n),
                        Tok::Ident { name, .. } => mods.push(name),
                        Tok::Op(o) if o == "-" => mods.push(o),
                        _ => break,
                    }
                }
                if !ok {
                    self.pos = save;
                    break;
                }
                out.push('(');
                out.push_str(&mods.concat());
                out.push(')');
                continue;
            }
            if multiword {
                let words = [
                    "varying",
                    "precision",
                    "with",
                    "without",
                    "time",
                    "zone",
                    "year",
                    "month",
                    "day",
                    "hour",
                    "minute",
                    "second",
                    "to",
                ];
                if let Some(Tok::Ident {
                    name,
                    quoted: false,
                }) = self.peek()
                    && words.contains(&name.as_str())
                {
                    out.push(' ');
                    out.push_str(name);
                    self.pos += 1;
                    continue;
                }
            }
            if self.peek() == Some(&Tok::LBracket) && self.peek_at(1) == Some(&Tok::RBracket) {
                self.pos += 2;
                out.push_str("[]");
                continue;
            }
            break;
        }
        Some(out)
    }

    fn list(&mut self, close: &Tok) -> Option<Vec<Expr>> {
        let mut items = Vec::new();
        if self.peek() == Some(close) {
            self.pos += 1;
            return Some(items);
        }
        loop {
            items.push(self.expr(0)?);
            match self.next()? {
                Tok::Comma => {}
                t if &t == close => return Some(items),
                _ => return None,
            }
        }
    }

    fn primary(&mut self) -> Option<Expr> {
        match self.next()? {
            Tok::Str(s) => Some(Expr::Lit(Lit::Str(s))),
            Tok::Num(n) => Some(Expr::Lit(Lit::Num(n))),
            Tok::LParen => {
                let items = self.list(&Tok::RParen)?;
                if items.len() == 1 {
                    items.into_iter().next()
                } else {
                    Some(Expr::Row(items))
                }
            }
            Tok::Op(op) if op == "-" => Some(Expr::Neg(Box::new(self.expr(11)?))),
            Tok::Op(op) if op == "+" => self.expr(11),
            Tok::Ident {
                name,
                quoted: false,
            } => self.word(&name),
            Tok::Ident { name, quoted: true } => self.name(name),
            _ => None,
        }
    }

    fn word(&mut self, word: &str) -> Option<Expr> {
        match word {
            "true" => Some(Expr::Lit(Lit::Bool(true))),
            "false" => Some(Expr::Lit(Lit::Bool(false))),
            "null" => Some(Expr::Lit(Lit::Null)),
            "value" if self.domain && self.peek() != Some(&Tok::LParen) => Some(Expr::Value),
            "not" => Some(Expr::Not(Box::new(self.expr(3)?))),
            "array" if self.peek() == Some(&Tok::LBracket) => {
                self.pos += 1;
                Some(Expr::Array(self.list(&Tok::RBracket)?))
            }
            "row" if self.peek() == Some(&Tok::LParen) => {
                self.pos += 1;
                Some(Expr::Row(self.list(&Tok::RParen)?))
            }
            "current_date" | "current_timestamp" | "localtimestamp" | "current_time"
            | "localtime" | "current_user" | "session_user" | "current_role" | "user"
            | "current_schema" | "current_catalog"
                if self.peek() != Some(&Tok::LParen) || word.contains("time") =>
            {
                // `CURRENT_TIMESTAMP(3)`: the precision is not a call.
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    self.list(&Tok::RParen)?;
                }
                Some(Expr::Keyword(word.to_owned()))
            }
            "case" => {
                // Skip to the matching END: not modelled.
                let mut depth = 1;
                while let Some(t) = self.next() {
                    match t {
                        Tok::Ident {
                            ref name,
                            quoted: false,
                        } if name == "case" => depth += 1,
                        Tok::Ident {
                            ref name,
                            quoted: false,
                        } if name == "end" => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(Expr::Unknown);
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            // A typed literal: `interval '1 day'`, `date '2020-01-01'`.
            "interval" | "date" | "timestamp" | "timestamptz" | "time"
                if matches!(self.peek(), Some(Tok::Str(_))) =>
            {
                let Some(Tok::Str(s)) = self.next() else {
                    return None;
                };
                Some(Expr::Cast(
                    Box::new(Expr::Lit(Lit::Str(s))),
                    word.to_owned(),
                ))
            }
            _ => self.name(word.to_owned()),
        }
    }

    /// A name: a column, or a call when `(` follows.
    fn name(&mut self, first: String) -> Option<Expr> {
        let mut parts = vec![first];
        while self.peek() == Some(&Tok::Dot) {
            self.pos += 1;
            match self.next()? {
                Tok::Ident { name, .. } => parts.push(name),
                Tok::Op(o) if o == "*" => return Some(Expr::Unknown),
                _ => return None,
            }
        }
        if self.peek() == Some(&Tok::LParen) {
            self.pos += 1;
            let name = parts.join(".");
            if name == "operator" {
                return None;
            }
            // `count(*)`, `f(VARIADIC x)`, `f(a => 1)`: not modelled.
            if matches!(self.peek(), Some(Tok::Op(o)) if o == "*") {
                self.pos += 1;
                self.expect(&Tok::RParen)?;
                return Some(Expr::Func(name, Vec::new()));
            }
            self.eat_word("variadic");
            self.eat_word("distinct");
            let args = self.list(&Tok::RParen)?;
            return Some(Expr::Func(name, args));
        }
        Some(Expr::Col(parts.pop()?))
    }

    /// The binding power of the operator at the cursor, if any.
    fn infix(&self) -> Option<(u8, Infix)> {
        let tok = self.peek()?;
        Some(match tok {
            Tok::Ident {
                name,
                quoted: false,
            } => match name.as_str() {
                "or" => (1, Infix::Or),
                "and" => (2, Infix::And),
                "is" => (4, Infix::Is),
                "isnull" => (4, Infix::IsNull(false)),
                "notnull" => (4, Infix::IsNull(true)),
                "between" => (6, Infix::Between(false)),
                "in" => (6, Infix::In(false)),
                "like" => (6, Infix::Word("~~")),
                "ilike" => (6, Infix::Word("~~*")),
                "collate" => (12, Infix::Collate),
                "at" if self.is_word_at(1, "time") => (12, Infix::AtTimeZone),
                "not" if self.is_word_at(1, "between") => (6, Infix::Between(true)),
                "not" if self.is_word_at(1, "in") => (6, Infix::In(true)),
                "not" if self.is_word_at(1, "like") => (6, Infix::NotWord("!~~")),
                "not" if self.is_word_at(1, "ilike") => (6, Infix::NotWord("!~~*")),
                "operator" if self.peek_at(1) == Some(&Tok::LParen) => (7, Infix::Operator),
                _ => return None,
            },
            Tok::Op(op) => match op.as_str() {
                "::" => (13, Infix::Cast),
                "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" => (5, Infix::Op(op.clone())),
                "+" | "-" => (8, Infix::Op(op.clone())),
                "*" | "/" | "%" => (9, Infix::Op(op.clone())),
                "^" => (10, Infix::Op(op.clone())),
                _ => (7, Infix::Op(op.clone())),
            },
            Tok::LBracket => (12, Infix::Subscript),
            _ => return None,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub fn expr(&mut self, min_bp: u8) -> Option<Expr> {
        let mut left = self.primary()?;
        while let Some((bp, infix)) = self.infix() {
            if bp < min_bp {
                break;
            }
            self.pos += 1;
            left = match infix {
                Infix::Or => {
                    let right = self.expr(bp + 1)?;
                    match left {
                        Expr::Or(mut list) => {
                            list.push(right);
                            Expr::Or(list)
                        }
                        other => Expr::Or(vec![other, right]),
                    }
                }
                Infix::And => {
                    let right = self.expr(bp + 1)?;
                    match left {
                        Expr::And(mut list) => {
                            list.push(right);
                            Expr::And(list)
                        }
                        other => Expr::And(vec![other, right]),
                    }
                }
                Infix::Is => {
                    let negated = self.eat_word("not");
                    if self.eat_word("null") {
                        Expr::IsNull(Box::new(left), negated)
                    } else if self.eat_word("true") {
                        Expr::IsBool(Box::new(left), true, negated)
                    } else if self.eat_word("false") {
                        Expr::IsBool(Box::new(left), false, negated)
                    } else if self.eat_word("distinct") {
                        if !self.eat_word("from") {
                            return None;
                        }
                        let right = self.expr(bp + 1)?;
                        let op = if negated {
                            "is not distinct from"
                        } else {
                            "is distinct from"
                        };
                        Expr::Bin(op.to_owned(), Box::new(left), Box::new(right))
                    } else {
                        // IS DOCUMENT, IS NORMALIZED…
                        while matches!(self.peek(), Some(Tok::Ident { .. })) {
                            self.pos += 1;
                        }
                        Expr::Unknown
                    }
                }
                Infix::IsNull(negated) => Expr::IsNull(Box::new(left), negated),
                Infix::Between(negated) => {
                    if negated {
                        self.pos += 1;
                    }
                    self.eat_word("symmetric");
                    let low = self.expr(7)?;
                    if !self.eat_word("and") {
                        return None;
                    }
                    let high = self.expr(7)?;
                    Expr::Between(Box::new(left), Box::new(low), Box::new(high), negated)
                }
                Infix::In(negated) => {
                    if negated {
                        self.pos += 1;
                    }
                    self.expect(&Tok::LParen)?;
                    let list = self.list(&Tok::RParen)?;
                    Expr::In(Box::new(left), list, negated)
                }
                Infix::Word(op) => {
                    let right = self.expr(bp + 1)?;
                    self.escape_clause();
                    Expr::Bin(op.to_owned(), Box::new(left), Box::new(right))
                }
                Infix::NotWord(op) => {
                    self.pos += 1;
                    let right = self.expr(bp + 1)?;
                    self.escape_clause();
                    Expr::Bin(op.to_owned(), Box::new(left), Box::new(right))
                }
                Infix::Cast => Expr::Cast(Box::new(left), self.type_name()?),
                Infix::Collate => {
                    // The collation name; the value is what matters.
                    self.type_name()?;
                    left
                }
                Infix::AtTimeZone => {
                    self.pos += 2;
                    let _zone = self.expr(bp + 1)?;
                    Expr::Unknown
                }
                Infix::Subscript => {
                    let mut depth = 1;
                    while let Some(t) = self.next() {
                        match t {
                            Tok::LBracket => depth += 1,
                            Tok::RBracket => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    Expr::Unknown
                }
                Infix::Operator => {
                    // OPERATOR(schema.op): the operator is what is left of it.
                    self.expect(&Tok::LParen)?;
                    let mut op = None;
                    while let Some(t) = self.next() {
                        match t {
                            Tok::RParen => break,
                            Tok::Op(o) => op = Some(o),
                            _ => {}
                        }
                    }
                    let op = op?;
                    self.binary(left, op, 7)?
                }
                Infix::Op(op) => self.binary(left, op, bp)?,
            };
        }
        Some(left)
    }

    fn binary(&mut self, left: Expr, op: String, bp: u8) -> Option<Expr> {
        // `x = ANY (…)`, `x <> ALL (…)`.
        if (self.is_word("any") || self.is_word("some") || self.is_word("all"))
            && self.peek_at(1) == Some(&Tok::LParen)
        {
            let all = self.is_word("all");
            self.pos += 2;
            let right = self.expr(0)?;
            self.expect(&Tok::RParen)?;
            return Some(Expr::Quantified {
                op,
                all,
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        let right = self.expr(bp + 1)?;
        Some(Expr::Bin(op, Box::new(left), Box::new(right)))
    }

    fn escape_clause(&mut self) {
        if self.eat_word("escape") {
            let _ = self.next();
        }
    }
}

enum Infix {
    Or,
    And,
    Is,
    IsNull(bool),
    Between(bool),
    In(bool),
    Word(&'static str),
    NotWord(&'static str),
    Cast,
    Collate,
    AtTimeZone,
    Subscript,
    Operator,
    Op(String),
}

/// The key columns of an index definition (`CREATE [UNIQUE] INDEX name ON
/// t USING m (a, lower(b)) [INCLUDE (…)] [NULLS NOT DISTINCT] [WHERE …]`):
/// each key's expression, and whether NULLs are not distinct.
pub fn index_keys(definition: &str) -> Option<(Vec<Expr>, bool)> {
    let toks = lex(definition)?;
    let mut p = Parser::new(toks);
    // Skip to the first `(` after USING (or after ON t).
    while let Some(t) = p.peek() {
        if *t == Tok::LParen {
            break;
        }
        p.pos += 1;
    }
    p.expect(&Tok::LParen)?;
    let mut keys = Vec::new();
    loop {
        keys.push(p.expr(0)?);
        // Opclass, ordering, collation and NULLS FIRST after a key.
        while let Some(Tok::Ident { .. }) = p.peek() {
            p.pos += 1;
            if p.peek() == Some(&Tok::Dot) {
                p.pos += 2;
            }
        }
        if p.peek() == Some(&Tok::LParen) {
            // Opclass parameters.
            let mut depth = 0;
            while let Some(t) = p.next() {
                match t {
                    Tok::LParen => depth += 1,
                    Tok::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        match p.next()? {
            Tok::Comma => {}
            Tok::RParen => break,
            _ => return None,
        }
    }
    let rest: Vec<String> = p.toks[p.pos..]
        .iter()
        .map(|t| match t {
            Tok::Ident { name, .. } => name.clone(),
            other => other.to_string(),
        })
        .collect();
    let nulls_not_distinct = rest.windows(3).any(|w| w == ["nulls", "not", "distinct"]);
    Some((keys, nulls_not_distinct))
}

/// The elements of `EXCLUDE USING m (a WITH =, b WITH &&)`: each element's
/// expression and operator.
pub fn exclusion_elements(definition: &str) -> Option<Vec<(Expr, String)>> {
    let toks = lex(definition)?;
    let mut p = Parser::new(toks);
    while let Some(t) = p.peek() {
        if *t == Tok::LParen {
            break;
        }
        p.pos += 1;
    }
    p.expect(&Tok::LParen)?;
    let mut out = Vec::new();
    loop {
        let e = p.expr(0)?;
        // Opclass before WITH.
        while !p.is_word("with") {
            match p.next()? {
                Tok::Ident { .. } | Tok::Dot => {}
                _ => return None,
            }
        }
        p.pos += 1;
        let op = match p.next()? {
            Tok::Op(o) => o,
            Tok::Ident { name, .. } if name == "operator" => {
                p.expect(&Tok::LParen)?;
                let mut op = None;
                while let Some(t) = p.next() {
                    match t {
                        Tok::RParen => break,
                        Tok::Op(o) => op = Some(o),
                        _ => {}
                    }
                }
                op?
            }
            _ => return None,
        };
        out.push((e, op));
        match p.next()? {
            Tok::Comma => {}
            Tok::RParen => break,
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_box_returns)]
    fn col(name: &str) -> Box<Expr> {
        Box::new(Expr::Col(name.to_owned()))
    }

    #[test]
    fn a_check_as_postgres_prints_it_parses() {
        let e = parse_check(
            "CHECK (((status = 'cart'::public.order_status) OR (placed_at IS NOT NULL)))",
            false,
        )
        .unwrap();
        let Expr::Or(list) = e else { panic!() };
        assert_eq!(
            list[0],
            Expr::Bin(
                "=".into(),
                col("status"),
                Box::new(Expr::Cast(
                    Box::new(Expr::Lit(Lit::Str("cart".into()))),
                    "public.order_status".into()
                ))
            )
        );
        assert_eq!(list[1], Expr::IsNull(col("placed_at"), true));
    }

    #[test]
    fn negative_constants_and_multiword_casts_parse() {
        let e = parse_check(
            "CHECK (((reading >= ('-1000'::integer)::double precision) AND (reading <= (1000)::double precision)))",
            false,
        )
        .unwrap();
        let conj = e.conjuncts();
        assert_eq!(conj.len(), 2);
        let Expr::Bin(op, _, right) = conj[0] else {
            panic!()
        };
        assert_eq!(op, ">=");
        let Expr::Cast(inner, ty) = &**right else {
            panic!()
        };
        assert_eq!(ty, "double precision");
        assert_eq!(
            **inner,
            Expr::Cast(
                Box::new(Expr::Lit(Lit::Str("-1000".into()))),
                "integer".into()
            )
        );
    }

    #[test]
    fn an_operator_written_qualified_reads_as_the_operator() {
        let e = parse_check(
            "CHECK ((VALUE OPERATOR(public.~) '^[^@]+@[^@]+$'::public.citext))",
            true,
        )
        .unwrap();
        let Expr::Bin(op, left, _) = e else { panic!() };
        assert_eq!(op, "~");
        assert_eq!(*left, Expr::Value);
    }

    #[test]
    fn any_array_and_functions_parse() {
        let e = parse(
            "(status = ANY (ARRAY['placed'::shop.order_status, 'shipped'::shop.order_status]))",
        )
        .unwrap();
        let Expr::Quantified { op, all, right, .. } = e else {
            panic!()
        };
        assert_eq!(op, "=");
        assert!(!all);
        assert!(matches!(*right, Expr::Array(ref items) if items.len() == 2));
        let e = parse_check("CHECK ((char_length(username) >= 3))", false).unwrap();
        assert_eq!(e.columns(), ["username"]);
        let e = parse_check("CHECK (public.st_isvalid(location))", false).unwrap();
        assert_eq!(e.calls(), ["public.st_isvalid"]);
    }

    #[test]
    fn quoted_names_keep_their_case() {
        let e = parse_check(r#"CHECK (("Total" >= (0)::numeric))"#, false).unwrap();
        assert_eq!(e.columns(), ["Total"]);
    }

    #[test]
    fn defaults_parse() {
        assert_eq!(
            parse("nextval('public.customers_id_seq'::regclass)").unwrap(),
            Expr::Func(
                "nextval".into(),
                vec![Expr::Cast(
                    Box::new(Expr::Lit(Lit::Str("public.customers_id_seq".into()))),
                    "regclass".into()
                )]
            )
        );
        assert_eq!(
            parse("CURRENT_DATE").unwrap(),
            Expr::Keyword("current_date".into())
        );
        assert!(matches!(
            parse("ARRAY['open'::public.ticket_state]").unwrap(),
            Expr::Array(_)
        ));
        assert!(
            matches!(parse("'00000000'::\"bit\"").unwrap(), Expr::Cast(_, ty) if ty == "\"bit\"")
        );
        assert!(matches!(parse("'{}'::text[]").unwrap(), Expr::Cast(_, ty) if ty == "text[]"));
    }

    #[test]
    fn index_keys_are_read() {
        let (keys, nnd) = index_keys(
            "CREATE UNIQUE INDEX orders_note_lower_idx ON shop.orders USING btree (lower(note)) WHERE (note IS NOT NULL)",
        )
        .unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].columns(), ["note"]);
        assert!(!nnd);
        let (keys, _) = index_keys(
            "CREATE UNIQUE INDEX i ON public.t USING btree (a DESC NULLS LAST, b text_pattern_ops) INCLUDE (c)",
        )
        .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].columns(), ["b"]);
    }

    #[test]
    fn exclusion_elements_are_read() {
        let elements =
            exclusion_elements("EXCLUDE USING gist (room_id WITH =, during WITH &&)").unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[1].1, "&&");
        assert_eq!(elements[1].0, Expr::Col("during".into()));
    }

    #[test]
    fn what_it_cannot_read_is_none_or_unknown() {
        assert!(
            parse("CASE WHEN a THEN 1 ELSE 2 END = 1")
                .unwrap()
                .has_unknown()
        );
        assert!(parse("'unterminated").is_none());
    }
}
