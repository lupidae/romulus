//! Rewriting deparsed SQL the way a rename changes what Postgres prints.
//!
//! Postgres stores expressions, keys and view queries by OID, so a rename
//! carries over to every dependent object: what changes is only how
//! `pg_get_*def` prints them afterwards. These functions predict that print
//! on the text the model holds, token by token: an identifier is renamed
//! where it stands for the renamed object (a qualified relation, a column
//! of the right table, a type after `::`), never inside a string literal or
//! where it names something else (a function, a type, another table's
//! column).
//!
//! A prediction that misses only makes the planner see a change and build
//! the object again; one that says too much could hide one. The rules lean
//! on what Postgres prints (qualified relations, since remus's
//! introspection empties `search_path`; bare columns only where one
//! relation is in scope; keywords upper case, identifiers lower case or
//! quoted), and what they cannot predict is reported (a view is then built
//! again, see [`view_column`]).

use remus_core::emit::ident::{quote_ident, quote_literal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tok {
    /// A bare identifier or keyword.
    Word,
    /// A `"quoted"` identifier.
    Quoted,
    /// A string literal, `E'…'` included.
    Str,
    Space,
    /// Anything else: one character of punctuation or an operator, a number.
    Other,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub kind: Tok,
    pub text: String,
}

impl Token {
    /// The identifier it is, unquoted: `"Users"` → `Users`, `users` → `users`.
    pub fn ident(&self) -> Option<String> {
        match self.kind {
            Tok::Word => Some(self.text.clone()),
            Tok::Quoted => Some(self.text[1..self.text.len() - 1].replace("\"\"", "\"")),
            _ => None,
        }
    }

    /// The identifier `name` as Postgres prints it is this token.
    pub fn is(&self, name: &str) -> bool {
        match self.kind {
            Tok::Word => self.text == name,
            Tok::Quoted => self.ident().as_deref() == Some(name),
            _ => false,
        }
    }

    fn is_punct(&self, c: &str) -> bool {
        self.kind == Tok::Other && self.text == c
    }

    /// An upper-case word: a keyword as Postgres prints them.
    fn is_keyword(&self) -> bool {
        self.kind == Tok::Word
            && self.text.chars().any(|c| c.is_ascii_uppercase())
            && !self.text.chars().any(|c| c.is_ascii_lowercase())
    }
}

pub(crate) fn tokens(sql: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let push = |out: &mut Vec<Token>, kind, text: String| out.push(Token { kind, text });
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push(&mut out, Tok::Space, chars[start..i].iter().collect());
        } else if c == '\''
            || ((c == 'E' || c == 'e') && chars.get(i + 1) == Some(&'\'') && !word_before(&out))
        {
            if c != '\'' {
                i += 1;
            }
            i += 1;
            // '' inside is an escaped quote; E'' strings may also escape it
            // with a backslash.
            let escapes = c != '\'';
            while i < chars.len() {
                if escapes && chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            push(
                &mut out,
                Tok::Str,
                chars[start..i.min(chars.len())].iter().collect(),
            );
        } else if c == '"' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            push(
                &mut out,
                Tok::Quoted,
                chars[start..i.min(chars.len())].iter().collect(),
            );
        } else if c.is_alphabetic() || c == '_' {
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            push(&mut out, Tok::Word, chars[start..i].iter().collect());
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            push(&mut out, Tok::Other, chars[start..i].iter().collect());
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            i += 2;
            push(&mut out, Tok::Other, "::".to_owned());
        } else {
            i += 1;
            push(&mut out, Tok::Other, c.to_string());
        }
    }
    out
}

/// The previous token is a word: `fooE'x'` is not an escape string.
fn word_before(out: &[Token]) -> bool {
    out.last().is_some_and(|t| t.kind == Tok::Word)
}

pub(crate) fn join(tokens: &[Token]) -> String {
    tokens.iter().map(|t| t.text.as_str()).collect()
}

/// The index of the next token that is not a space, after `i`.
fn next(tokens: &[Token], i: usize) -> Option<usize> {
    (i + 1..tokens.len()).find(|&j| tokens[j].kind != Tok::Space)
}

/// The index of the previous token that is not a space, before `i`.
fn prev(tokens: &[Token], i: usize) -> Option<usize> {
    (0..i).rev().find(|&j| tokens[j].kind != Tok::Space)
}

fn ident_token(name: &str) -> Token {
    let text = quote_ident(name).into_owned();
    let kind = if text.starts_with('"') {
        Tok::Quoted
    } else {
        Tok::Word
    };
    Token { kind, text }
}

/// `tokens[i]` is `schema` `.` `name`'s last part: `public.users` at `users`.
fn qualified_at(tokens: &[Token], i: usize, schema: &str, name: &str) -> bool {
    tokens[i].is(name)
        && prev(tokens, i).is_some_and(|d| {
            tokens[d].is_punct(".") && prev(tokens, d).is_some_and(|s| tokens[s].is(schema))
        })
}

/// Renames the relation `schema.from` to `to`: where it is named qualified
/// (`public.users`), and where its name qualifies a column (`users.email`,
/// which Postgres prints with the relation's current name when the query
/// gave it no alias).
pub(crate) fn rename_relation(sql: &str, schema: &str, from: &str, to: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if !t[i].is(from) {
            continue;
        }
        let qualified = qualified_at(&t, i, schema, from);
        // A qualifier: followed by `.`, not itself after one.
        let qualifier = next(&t, i).is_some_and(|d| t[d].is_punct("."))
            && !prev(&t, i).is_some_and(|d| t[d].is_punct("."));
        if qualified || qualifier {
            t[i] = ident_token(to);
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// Moves the relation `schema.name` to `to_schema` where it is named
/// qualified (`public.users` → `archive.users`).
pub(crate) fn move_relation(sql: &str, schema: &str, name: &str, to_schema: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if !qualified_at(&t, i, schema, name) {
            continue;
        }
        if let Some(dot) = prev(&t, i)
            && let Some(s) = prev(&t, dot)
        {
            t[s] = ident_token(to_schema);
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// Moves the type `schema.name` to `to_schema` where a cast names it
/// qualified (`'a'::public.mood` → `'a'::archive.mood`), and in a type's
/// own text (`public.mood[]`).
pub(crate) fn move_type(sql: &str, schema: &str, name: &str, to_schema: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if !qualified_at(&t, i, schema, name) {
            continue;
        }
        let Some(dot) = prev(&t, i) else { continue };
        let Some(s) = prev(&t, dot) else { continue };
        let cast = prev(&t, s).is_some_and(|c| t[c].is_punct("::"));
        let alone = prev(&t, s).is_none();
        if cast || alone {
            t[s] = ident_token(to_schema);
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// The sequences of `schema` a default draws from, moved to `to_schema`
/// (a table's own sequences move with it).
pub(crate) fn move_sequences(sql: &str, schema: &str, to_schema: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if t[i].kind != Tok::Str || !t[i].text.starts_with('\'') {
            continue;
        }
        let Some(c) = next(&t, i) else { continue };
        let regclass = t[c].is_punct("::")
            && next(&t, c).is_some_and(|r| t[r].kind == Tok::Word && t[r].text == "regclass");
        if !regclass {
            continue;
        }
        let inner = t[i].text[1..t[i].text.len() - 1].replace("''", "'");
        if let Some((Some(s), name)) = regclass_name(&inner)
            && s == schema
        {
            let moved = format!("{}.{}", quote_ident(to_schema), quote_ident(&name));
            t[i] = Token {
                kind: Tok::Str,
                text: quote_literal(&moved),
            };
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// What a bare identifier at `i` can be renamed as a column: not a function
/// (`lower(`), not a type (after `::`), not qualified (before or after a
/// `.`), not a keyword.
fn bare_column_at(t: &[Token], i: usize) -> bool {
    if t[i].kind == Tok::Word && t[i].is_keyword() {
        return false;
    }
    let before = prev(t, i);
    let after = next(t, i);
    !before.is_some_and(|d| t[d].is_punct(".") || t[d].is_punct("::"))
        && !after.is_some_and(|d| t[d].is_punct(".") || t[d].is_punct("("))
}

/// Renames the column `from` where it stands bare: in an expression of its
/// own table (a check, an index, a policy, a generated column, a partition
/// key), where Postgres prints its columns unqualified.
pub(crate) fn rename_bare_column(sql: &str, from: &str, to: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if t[i].is(from) && bare_column_at(&t, i) {
            t[i] = ident_token(to);
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// The names `schema.table` goes by in `sql`, when it reads the table
/// (names it qualified, as Postgres prints relations): its own (the
/// qualifier Postgres prints without an alias) and every alias it is given
/// (`FROM public.events e`). Empty when the text does not read it.
pub(crate) fn qualifiers(sql: &str, schema: &str, table: &str) -> Vec<String> {
    let t = tokens(sql);
    let mut out = Vec::new();
    for i in 0..t.len() {
        if !qualified_at(&t, i, schema, table) {
            continue;
        }
        out.push(table.to_owned());
        let Some(mut j) = next(&t, i) else { continue };
        if t[j].kind == Tok::Word && t[j].text == "AS" {
            let Some(k) = next(&t, j) else { continue };
            j = k;
        }
        let alias = &t[j];
        let is_alias = match alias.kind {
            Tok::Quoted => true,
            Tok::Word => !alias.is_keyword(),
            _ => false,
        };
        if is_alias && let Some(name) = alias.ident() {
            out.push(name);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Renames the column `from` where it is qualified by one of `qualifiers`
/// (`users.email`, `u.email`): in a view, or in another table's policy.
pub(crate) fn rename_qualified_column(
    sql: &str,
    qualifiers: &[String],
    from: &str,
    to: &str,
) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if !t[i].is(from) {
            continue;
        }
        let qualified = prev(&t, i).is_some_and(|d| {
            t[d].is_punct(".")
                && prev(&t, d).is_some_and(|q| {
                    qualifiers.iter().any(|name| t[q].is(name))
                        && !prev(&t, q).is_some_and(|x| t[x].is_punct("."))
                })
        });
        // `(row).field` is not a column of the table.
        if qualified && !next(&t, i).is_some_and(|d| t[d].is_punct(".")) {
            t[i] = ident_token(to);
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// Renames the type `schema.from` where it stands as a type: after `::`,
/// qualified or not (`'a'::public.mood`, `x::mood[]`).
pub(crate) fn rename_type(sql: &str, schema: &str, from: &str, to: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if !t[i].is(from) {
            continue;
        }
        let Some(d) = prev(&t, i) else { continue };
        let cast = t[d].is_punct("::");
        let qualified_cast = t[d].is_punct(".")
            && prev(&t, d).is_some_and(|s| {
                t[s].is(schema) && prev(&t, s).is_some_and(|c| t[c].is_punct("::"))
            });
        if cast || qualified_cast {
            t[i] = ident_token(to);
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// A column's type as `format_type` prints it (`mood`, `public.mood`,
/// `mood[]`, `"Mood"`), with the type renamed.
pub(crate) fn rename_type_name(text: &str, schema: &str, from: &str, to: &str) -> String {
    let mut t = tokens(text);
    let mut changed = false;
    for i in 0..t.len() {
        if !t[i].is(from) {
            continue;
        }
        let first = prev(&t, i).is_none();
        if first || qualified_at(&t, i, schema, from) {
            t[i] = ident_token(to);
            changed = true;
        }
    }
    if changed { join(&t) } else { text.to_owned() }
}

/// The type a cast names, after the `::` at `at`: `(schema, name)`, the
/// schema `None` when it is not qualified.
fn cast_type(t: &[Token], at: usize) -> Option<(Option<String>, String)> {
    let first = next(t, at)?;
    let name = t[first].ident()?;
    match next(t, first) {
        Some(dot) if t[dot].is_punct(".") => {
            let second = next(t, dot)?;
            Some((Some(name), t[second].ident()?))
        }
        _ => Some((None, name)),
    }
}

/// Renames the enum label `from` to `to` in the literals cast to the type
/// `schema.ty` (`'paid'::public.status`): Postgres stores an enum constant
/// as its label's OID, which `RENAME VALUE` keeps.
pub(crate) fn rename_label(sql: &str, schema: &str, ty: &str, from: &str, to: &str) -> String {
    let mut t = tokens(sql);
    let literal = quote_literal(from);
    let mut changed = false;
    for i in 0..t.len() {
        if t[i].kind != Tok::Str || t[i].text != literal {
            continue;
        }
        let Some(c) = next(&t, i) else { continue };
        if !t[c].is_punct("::") {
            continue;
        }
        if let Some((s, name)) = cast_type(&t, c)
            && name == ty
            && s.as_deref().is_none_or(|s| s == schema)
        {
            t[i] = Token {
                kind: Tok::Str,
                text: quote_literal(to),
            };
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// A `regclass` literal's relation: `'public.users_id_seq'` →
/// `("public", "users_id_seq")`, read as Postgres reads it (bare names
/// folded to lower case).
pub(crate) fn regclass_name(literal: &str) -> Option<(Option<String>, String)> {
    let t = tokens(literal);
    let parts: Vec<&Token> = t.iter().filter(|x| x.kind != Tok::Space).collect();
    let folded = |token: &Token| {
        token.ident().map(|name| {
            if token.kind == Tok::Word {
                name.to_lowercase()
            } else {
                name
            }
        })
    };
    match parts.as_slice() {
        [name] => Some((None, folded(name)?)),
        [schema, dot, name] if dot.is_punct(".") => Some((Some(folded(schema)?), folded(name)?)),
        _ => None,
    }
}

/// The sequence `schema.from` renamed in `nextval('…'::regclass)` literals.
pub(crate) fn rename_sequence(sql: &str, schema: &str, from: &str, to: &str) -> String {
    let mut t = tokens(sql);
    let mut changed = false;
    for i in 0..t.len() {
        if t[i].kind != Tok::Str || !t[i].text.starts_with('\'') {
            continue;
        }
        let Some(c) = next(&t, i) else { continue };
        let regclass = t[c].is_punct("::")
            && next(&t, c).is_some_and(|r| t[r].kind == Tok::Word && t[r].text == "regclass");
        if !regclass {
            continue;
        }
        let inner = t[i].text[1..t[i].text.len() - 1].replace("''", "'");
        let Some((s, name)) = regclass_name(&inner) else {
            continue;
        };
        if name == from && s.as_deref().is_none_or(|s| s == schema) {
            let renamed = match s {
                Some(s) => format!("{}.{}", quote_ident(&s), quote_ident(to)),
                None => quote_ident(to).into_owned(),
            };
            t[i] = Token {
                kind: Tok::Str,
                text: quote_literal(&renamed),
            };
            changed = true;
        }
    }
    if changed { join(&t) } else { sql.to_owned() }
}

/// The name of the index a `CREATE INDEX` statement creates, renamed.
pub(crate) fn rename_index_definition(definition: &str, from: &str, to: &str) -> String {
    let mut t = tokens(definition);
    for i in 0..t.len() {
        if t[i].kind == Tok::Word && t[i].text == "ON" {
            break;
        }
        if t[i].is(from) {
            t[i] = ident_token(to);
            return join(&t);
        }
    }
    definition.to_owned()
}

/// `CREATE INDEX name ON …` with the name blanked: two indexes alike but
/// for their names compare equal.
pub(crate) fn index_shape(definition: &str, name: &str) -> String {
    crate::sql::squash(&rename_index_definition(definition, name, "\u{1}"))
}

/// What follows `USING method` in an index definition: the part that names
/// the table's columns. `None` when the text is not shaped like one.
pub(crate) fn index_columns_start(t: &[Token]) -> Option<usize> {
    let using = t
        .iter()
        .position(|x| x.kind == Tok::Word && x.text == "USING")?;
    next(t, using).and_then(|method| next(t, method))
}

/// Renames the column `from` of the indexed table in an index definition
/// (its key, `INCLUDE` and `WHERE` parts).
pub(crate) fn rename_index_column(definition: &str, from: &str, to: &str) -> String {
    let mut t = tokens(definition);
    let Some(start) = index_columns_start(&t) else {
        return definition.to_owned();
    };
    let mut changed = false;
    for i in start..t.len() {
        if t[i].is(from) && bare_column_at(&t, i) {
            t[i] = ident_token(to);
            changed = true;
        }
    }
    if changed {
        join(&t)
    } else {
        definition.to_owned()
    }
}

/// A foreign key's definition split at `REFERENCES`: its own columns, then
/// the target and its columns.
fn split_references(definition: &str) -> Option<(String, String)> {
    let t = tokens(definition);
    let at = t
        .iter()
        .position(|x| x.kind == Tok::Word && x.text == "REFERENCES")?;
    Some((join(&t[..at]), join(&t[at..])))
}

/// Renames a column of a constraint's own table in its definition: every
/// bare mention, except in a foreign key's target part (the other table's
/// columns), unless the key references its own table (`self`).
pub(crate) fn rename_constraint_column(
    definition: &str,
    from: &str,
    to: &str,
    self_reference: bool,
) -> String {
    match split_references(definition) {
        Some((own, target)) => {
            let target = if self_reference {
                rename_reference_column(&target, from, to)
            } else {
                target
            };
            format!("{}{target}", rename_bare_column(&own, from, to))
        }
        None => rename_bare_column(definition, from, to),
    }
}

/// Renames a referenced column in a foreign key's `REFERENCES t(a, b) …`
/// part (or a whole definition): only inside the parentheses after the
/// table.
pub(crate) fn rename_reference_column(text: &str, from: &str, to: &str) -> String {
    let (head, target) = match split_references(text) {
        Some((own, target)) => (own, target),
        None => (String::new(), text.to_owned()),
    };
    let mut t = tokens(&target);
    let Some(open) = t.iter().position(|x| x.is_punct("(")) else {
        return text.to_owned();
    };
    let mut changed = false;
    for token in &mut t[open..] {
        if token.is_punct(")") {
            break;
        }
        if token.is(from) {
            *token = ident_token(to);
            changed = true;
        }
    }
    if changed {
        format!("{head}{}", join(&t))
    } else {
        text.to_owned()
    }
}

/// How a view over a renamed column prints, when that can be told.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ViewRewrite {
    /// The definition as Postgres prints it after the rename.
    Text(String),
    /// Not a single `SELECT` this can follow (a `UNION`, a `WITH`, a
    /// subquery): the view is built again around the rename.
    Unknown,
}

/// A view's definition after its table's column `from` is renamed `to`.
/// Postgres keeps the view's own output names: an output column that was
/// the renamed column itself prints `to AS from`. `bare` when the view
/// reads one relation, whose columns Postgres prints unqualified.
pub(crate) fn view_column(
    definition: &str,
    qualifiers: &[String],
    bare: bool,
    from: &str,
    to: &str,
) -> ViewRewrite {
    let t = tokens(definition);
    let selects = t
        .iter()
        .filter(|x| x.kind == Tok::Word && x.text == "SELECT")
        .count();
    let compound = t.iter().any(|x| {
        x.kind == Tok::Word && matches!(x.text.as_str(), "UNION" | "INTERSECT" | "EXCEPT" | "WITH")
    });
    if selects != 1 || compound {
        return ViewRewrite::Unknown;
    }
    let Some(select) = t
        .iter()
        .position(|x| x.kind == Tok::Word && x.text == "SELECT")
    else {
        return ViewRewrite::Unknown;
    };
    // The target list: up to the FROM at depth 0.
    let mut depth = 0_i32;
    let mut end = t.len();
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut item_start = select + 1;
    for (i, x) in t.iter().enumerate().skip(select + 1) {
        if x.is_punct("(") || x.is_punct("[") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") {
            depth -= 1;
        } else if depth == 0 && x.is_punct(",") {
            items.push((item_start, i));
            item_start = i + 1;
        } else if depth == 0 && x.kind == Tok::Word && x.text == "FROM" {
            end = i;
            break;
        }
    }
    items.push((item_start, end));

    let is_column = |slice: &[Token]| -> bool {
        let parts: Vec<&Token> = slice.iter().filter(|x| x.kind != Tok::Space).collect();
        match parts.as_slice() {
            [name] => bare && name.is(from),
            [q, dot, name] => {
                dot.is_punct(".") && name.is(from) && qualifiers.iter().any(|n| q.is(n))
            }
            _ => false,
        }
    };
    let mut out: Vec<Token> = Vec::with_capacity(t.len() + 4);
    out.extend(t[..=select].iter().cloned());
    let mut last = select + 1;
    for (start, stop) in items {
        out.extend(t[last..start].iter().cloned());
        let slice = &t[start..stop];
        let alias_needed = is_column(slice);
        let renamed = rename_in(slice, qualifiers, bare, from, to);
        // Keep the item's trailing space after the alias.
        let trailing = renamed
            .iter()
            .rposition(|x| x.kind != Tok::Space)
            .map_or(0, |p| p + 1);
        out.extend(renamed[..trailing].iter().cloned());
        if alias_needed {
            out.push(Token {
                kind: Tok::Space,
                text: " ".to_owned(),
            });
            out.push(Token {
                kind: Tok::Word,
                text: "AS".to_owned(),
            });
            out.push(Token {
                kind: Tok::Space,
                text: " ".to_owned(),
            });
            out.push(ident_token(from));
        }
        out.extend(renamed[trailing..].iter().cloned());
        last = stop;
    }
    out.extend(rename_in(&t[last..], qualifiers, bare, from, to));
    ViewRewrite::Text(join(&out))
}

/// The column renamed in a slice of a view's tokens: qualified by one of
/// the view's names for the table, or bare when it reads only that table.
fn rename_in(
    slice: &[Token],
    qualifiers: &[String],
    bare: bool,
    from: &str,
    to: &str,
) -> Vec<Token> {
    let text = join(slice);
    let text = rename_qualified_column(&text, qualifiers, from, to);
    let text = if bare {
        rename_bare_column(&text, from, to)
    } else {
        text
    };
    tokens(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_the_text() {
        let sql = "SELECT u.\"Name\", 'it''s'::text, E'a\\'b' FROM public.users u WHERE (x >= 1.5)";
        assert_eq!(join(&tokens(sql)), sql);
    }

    #[test]
    fn a_relation_is_renamed_where_it_is_one() {
        let def = "SELECT users.id FROM public.users WHERE (users.id > 0) AND 'users' <> users_x";
        assert_eq!(
            rename_relation(def, "public", "users", "accounts"),
            "SELECT accounts.id FROM public.accounts WHERE (accounts.id > 0) AND 'users' <> users_x"
        );
        // Another schema's table of that name is not this one.
        assert_eq!(
            rename_relation(
                "FOREIGN KEY (a) REFERENCES shop.users(id)",
                "public",
                "users",
                "x"
            ),
            "FOREIGN KEY (a) REFERENCES shop.users(id)"
        );
    }

    #[test]
    fn a_bare_column_is_renamed_but_not_a_function_a_type_or_a_qualified_name() {
        assert_eq!(
            rename_bare_column(
                "CHECK ((lower(email) <> ''::text) AND (o.email IS NULL) AND email)",
                "email",
                "mail"
            ),
            "CHECK ((lower(mail) <> ''::text) AND (o.email IS NULL) AND mail)"
        );
        assert_eq!(
            rename_bare_column("lower(text)", "lower", "x"),
            "lower(text)"
        );
        assert_eq!(
            rename_bare_column("(code)::text", "text", "x"),
            "(code)::text"
        );
        assert_eq!(
            rename_bare_column("\"Email\" = email", "Email", "E-mail"),
            "\"E-mail\" = email"
        );
    }

    #[test]
    fn aliases_are_found_after_the_relation() {
        let def = "SELECT 1 FROM public.events e JOIN public.users ON true JOIN public.events AS \"E\" ON true";
        assert_eq!(qualifiers(def, "public", "events"), ["E", "e", "events"]);
        assert_eq!(qualifiers(def, "public", "users"), ["users"]);
    }

    #[test]
    fn a_qualified_column_is_renamed_through_its_aliases() {
        let def = "EXISTS ( SELECT 1 FROM public.events e WHERE (e.id = notes.event_id))";
        let q = qualifiers(def, "public", "events");
        assert_eq!(
            rename_qualified_column(def, &q, "id", "event_id"),
            "EXISTS ( SELECT 1 FROM public.events e WHERE (e.event_id = notes.event_id))"
        );
        // A field of a composite column is not the table's column.
        assert_eq!(
            rename_qualified_column("(e.id).x", &q, "id", "y"),
            "(e.y).x"
        );
        assert_eq!(rename_qualified_column("e.id.x", &q, "id", "y"), "e.id.x");
    }

    #[test]
    fn types_are_renamed_after_casts_only() {
        assert_eq!(
            rename_type(
                "(mood = 'a'::public.mood) AND (x::mood[] IS NULL) AND mood('x')",
                "public",
                "mood",
                "feeling"
            ),
            "(mood = 'a'::public.feeling) AND (x::feeling[] IS NULL) AND mood('x')"
        );
        assert_eq!(
            rename_type_name("public.mood[]", "public", "mood", "f"),
            "public.f[]"
        );
        assert_eq!(
            rename_type_name("mood", "public", "mood", "Feel"),
            "\"Feel\""
        );
        assert_eq!(
            rename_type_name("character varying(3)", "public", "mood", "x"),
            "character varying(3)"
        );
    }

    #[test]
    fn labels_are_renamed_in_literals_of_their_type() {
        let sql = "(state = 'paid'::public.status) OR (note = 'paid'::text) OR (s = ANY (ARRAY['paid'::status]))";
        assert_eq!(
            rename_label(sql, "public", "status", "paid", "settled"),
            "(state = 'settled'::public.status) OR (note = 'paid'::text) OR (s = ANY (ARRAY['settled'::status]))"
        );
    }

    #[test]
    fn moves_rewrite_qualified_names_casts_and_owned_sequences() {
        assert_eq!(
            move_relation(
                "SELECT users.id FROM public.users JOIN shop.users s ON true",
                "public",
                "users",
                "archive"
            ),
            "SELECT users.id FROM archive.users JOIN shop.users s ON true"
        );
        assert_eq!(
            move_type(
                "(m = 'a'::public.mood) AND public.mood(x)",
                "public",
                "mood",
                "feelings"
            ),
            "(m = 'a'::feelings.mood) AND public.mood(x)"
        );
        assert_eq!(
            move_type("public.mood[]", "public", "mood", "f"),
            "f.mood[]"
        );
        assert_eq!(
            move_sequences(
                "nextval('public.users_id_seq'::regclass)",
                "public",
                "archive"
            ),
            "nextval('archive.users_id_seq'::regclass)"
        );
    }

    #[test]
    fn sequences_are_renamed_in_nextval() {
        assert_eq!(
            rename_sequence(
                "nextval('public.users_id_seq'::regclass)",
                "public",
                "users_id_seq",
                "accounts_id_seq"
            ),
            "nextval('public.accounts_id_seq'::regclass)"
        );
        assert_eq!(
            rename_sequence(
                "nextval('public.\"Users_id_seq\"'::regclass)",
                "public",
                "Users_id_seq",
                "a"
            ),
            "nextval('public.a'::regclass)"
        );
        assert_eq!(
            regclass_name("Public.Users"),
            Some((Some("public".into()), "users".into()))
        );
    }

    #[test]
    fn index_definitions_rename_their_name_and_their_columns() {
        let def = "CREATE UNIQUE INDEX users_email ON public.users USING btree (lower(email)) WHERE (email IS NOT NULL)";
        assert_eq!(
            rename_index_column(def, "email", "mail"),
            "CREATE UNIQUE INDEX users_email ON public.users USING btree (lower(mail)) WHERE (mail IS NOT NULL)"
        );
        assert_eq!(
            rename_index_definition(def, "users_email", "users_mail"),
            def.replace("users_email ON", "users_mail ON")
        );
        assert_eq!(
            index_shape(def, "users_email"),
            index_shape(&def.replace("users_email ON", "x ON"), "x")
        );
    }

    #[test]
    fn foreign_keys_rename_their_own_columns_and_their_targets_apart() {
        let def = "FOREIGN KEY (id) REFERENCES public.users(id) ON DELETE CASCADE";
        assert_eq!(
            rename_constraint_column(def, "id", "user_id", false),
            "FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE"
        );
        assert_eq!(
            rename_constraint_column(def, "id", "uid", true),
            "FOREIGN KEY (uid) REFERENCES public.users(uid) ON DELETE CASCADE"
        );
        assert_eq!(
            rename_reference_column(def, "id", "uid"),
            "FOREIGN KEY (id) REFERENCES public.users(uid) ON DELETE CASCADE"
        );
    }

    #[test]
    fn a_view_keeps_its_output_names_when_a_column_is_renamed() {
        let def = " SELECT id,\n    code,\n    lower(code) AS low,\n    code AS c\n   FROM public.accounts\n  WHERE (code <> ''::text);";
        let q = vec!["accounts".to_owned()];
        assert_eq!(
            view_column(def, &q, true, "code", "kode"),
            ViewRewrite::Text(
                " SELECT id,\n    kode AS code,\n    lower(kode) AS low,\n    kode AS c\n   FROM public.accounts\n  WHERE (kode <> ''::text);".to_owned()
            )
        );
        let joined = " SELECT a.id,\n    a.code\n   FROM (public.accounts a\n     JOIN public.b ON ((b.id = a.id)));";
        let q = qualifiers(joined, "public", "accounts");
        assert_eq!(
            view_column(joined, &q, false, "code", "kode"),
            ViewRewrite::Text(" SELECT a.id,\n    a.kode AS code\n   FROM (public.accounts a\n     JOIN public.b ON ((b.id = a.id)));".to_owned())
        );
        assert_eq!(
            view_column(
                " SELECT x FROM public.a UNION SELECT x FROM public.b;",
                &q,
                true,
                "x",
                "y"
            ),
            ViewRewrite::Unknown
        );
    }
}
