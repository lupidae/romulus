//! Correcting a stub from the error Postgres raised about it.
//!
//! The inference in [`crate::stubs`] guesses types from the text; Postgres
//! knows them. When a call to a stub fails, its error names the types it
//! wanted, and the compiler retries with those:
//!
//! | SQLSTATE | message | correction |
//! |---|---|---|
//! | 42883 | `function f(uuid, unknown) does not exist` | arguments `(uuid, text)` |
//! | 42883 | `operator does not exist: uuid = text` | the stub returns `uuid` |
//! | 42804 | `… is of type T but default expression is of type U` | returns `T` |
//! | 42804 | `argument of CHECK must be type boolean, not type text` | returns `boolean` |
//!
//! Message text is never written into SQL: a type read from a message is
//! accepted only if it re-parses as one lone type name, and is then written
//! back from the parse tree ([`SqlType::parse`]). A message that does not
//! (a type name crafted to look like SQL, say) teaches nothing, and the
//! compile reports Postgres's error as it is.

use crate::{
    names::SqlType,
    parse::ParsedSql,
    prepare::Prepared,
    stubs::{Overrides, Stub},
};

/// What the compiler kept of a Postgres error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub sqlstate: String,
    pub message: String,
    /// Byte offset in the script.
    pub position: Option<usize>,
}

/// Learn a correction from a failure; `true` when the overrides changed (so
/// another attempt can succeed).
pub(crate) fn learn(
    failure: &Failure,
    statement: Option<usize>,
    prepared: &Prepared,
    parsed: &ParsedSql,
    sql: &str,
    overrides: &mut Overrides,
) -> bool {
    let located = failure.position.map(|p| prepared.locate(p, parsed));
    let statement = located.as_ref().and_then(|l| l.statement).or(statement);
    let at = located.and_then(|l| l.source).map(|range| range.start);
    match failure.sqlstate.as_str() {
        "42883" => {
            if let Some((name, args)) = function_signature(&failure.message) {
                let stub = at
                    .and_then(|at| stub_called_at(prepared, at))
                    .or_else(|| stub_named(&prepared.stubs, &name, args.len()));
                let Some(stub) = stub else { return false };
                let args: Option<Vec<SqlType>> = args
                    .iter()
                    .map(|a| {
                        if a == "unknown" {
                            Some(SqlType::builtin("text"))
                        } else {
                            SqlType::parse(a)
                        }
                    })
                    .collect();
                return args.is_some_and(|args| {
                    !stub.has_args(&args) && overrides.set_args(&stub.schema, &stub.name, args)
                });
            }
            if let Some(rest) = failure.message.strip_prefix("operator does not exist: ") {
                let Some((left, operator, right)) = split_operator(rest) else {
                    return false;
                };
                // The operator's position when Postgres gives it (CREATE
                // POLICY does not): else every use of that operator in the
                // statement that failed.
                let span = statement
                    .and_then(|i| parsed.statements.get(i))
                    .map(|s| s.span);
                let candidates = prepared.operands.iter().filter(|o| match at {
                    Some(at) => o.operator == at,
                    None => span.is_some_and(|s| {
                        s.start <= o.operator
                            && o.operator < s.end
                            && parsed.token_at(o.operator).map(|t| &sql[t]) == Some(operator)
                    }),
                });
                for operand in candidates {
                    let Some(stub) = prepared.stubs.get(operand.stub) else {
                        continue;
                    };
                    let wanted = if operand.left { right } else { left };
                    if wanted == "unknown" {
                        continue;
                    }
                    let Some(wanted) = SqlType::parse(wanted) else {
                        return false;
                    };
                    if &wanted != stub.returns_type() {
                        return overrides.set_returns(
                            &stub.schema,
                            &stub.name,
                            stub.arity(),
                            wanted,
                        );
                    }
                }
            }
            false
        }
        "42804" => {
            let Some(wanted) = wanted_type(&failure.message).and_then(|w| SqlType::parse(&w))
            else {
                return false;
            };
            at.and_then(|at| stub_called_at(prepared, at))
                .is_some_and(|stub| {
                    &wanted != stub.returns_type()
                        && overrides.set_returns(&stub.schema, &stub.name, stub.arity(), wanted)
                })
        }
        _ => false,
    }
}

fn stub_called_at(prepared: &Prepared, at: usize) -> Option<&Stub> {
    let call = prepared.calls.iter().find(|c| c.location == at)?;
    prepared.stubs.get(call.stub)
}

fn stub_named<'a>(stubs: &'a [Stub], name: &str, arity: usize) -> Option<&'a Stub> {
    let (schema, name) = match name.rsplit_once('.') {
        Some((schema, name)) => (Some(unquote(schema)), unquote(name)),
        None => (None, unquote(name)),
    };
    stubs.iter().find(|s| {
        s.name == name && s.arity() == arity && schema.as_ref().is_none_or(|q| *q == s.schema)
    })
}

/// `uuid = text` → (`uuid`, `=`, `text`); `timestamp with time zone < date`
/// works too: the operator is the word made of operator characters.
fn split_operator(rest: &str) -> Option<(&str, &str, &str)> {
    let is_operator = |w: &str| !w.is_empty() && w.chars().all(|c| "+-*/<>=~!@#%^&|`?".contains(c));
    let words: Vec<(usize, &str)> = rest
        .split(' ')
        .scan(0, |offset, word| {
            let start = *offset;
            *offset += word.len() + 1;
            Some((start, word))
        })
        .collect();
    let (start, operator) = *words.iter().find(|(_, w)| is_operator(w))?;
    let left = rest[..start].trim();
    let right = rest[start + operator.len()..].trim();
    Some((left, operator, right))
}

/// `function public.f(integer, unknown) does not exist` → (`public.f`,
/// [`integer`, `unknown`]).
fn function_signature(message: &str) -> Option<(String, Vec<String>)> {
    let signature = message
        .strip_prefix("function ")?
        .strip_suffix(" does not exist")?;
    let open = signature.find('(')?;
    let name = signature[..open].to_owned();
    let inner = signature[open + 1..].strip_suffix(')')?;
    Some((name, split_types(inner)))
}

/// Split a comma-separated type list, respecting parentheses and quotes.
fn split_types(list: &str) -> Vec<String> {
    let mut types = Vec::new();
    let mut depth = 0_i32;
    let mut quoted = false;
    let mut current = String::new();
    for c in list.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                types.push(current.trim().to_owned());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        types.push(current.trim().to_owned());
    }
    types
}

/// The type a 42804 message says was wanted.
fn wanted_type(message: &str) -> Option<String> {
    // "column "x" is of type T but default expression is of type U"
    if let Some((before, _)) = message.split_once(" but ")
        && let Some((_, wanted)) = before.rsplit_once(" is of type ")
    {
        return Some(wanted.to_owned());
    }
    // "argument of CHECK must be type boolean, not type text"
    let (_, rest) = message.split_once(" must be type ")?;
    let (wanted, _) = rest.split_once(", not type ")?;
    Some(wanted.to_owned())
}

fn unquote(name: &str) -> String {
    name.strip_prefix('"')
        .and_then(|n| n.strip_suffix('"'))
        .map_or_else(|| name.to_owned(), |n| n.replace("\"\"", "\""))
}

#[cfg(test)]
mod tests {
    use super::{Failure, function_signature, learn, split_types, wanted_type};
    use crate::{catalog::Catalog, parse, prepare::prepare, stubs::Overrides};

    #[test]
    fn operators_are_found_between_multi_word_types() {
        assert_eq!(
            super::split_operator("uuid = text"),
            Some(("uuid", "=", "text"))
        );
        assert_eq!(
            super::split_operator("timestamp with time zone <@ tstzrange"),
            Some(("timestamp with time zone", "<@", "tstzrange"))
        );
        assert_eq!(super::split_operator("- text"), Some(("", "-", "text")));
        assert_eq!(super::split_operator("nothing here"), None);
    }

    #[test]
    fn an_operator_mismatch_without_a_position_uses_the_failing_statement() {
        let sql = "CREATE TABLE t (n numeric);\nCREATE POLICY p ON t USING (app.level() = (SELECT max(n) FROM t));";
        let parsed = parse(sql);
        let prepared = prepare(sql, &parsed, &Catalog::default(), &Overrides::default());
        let failure = Failure {
            sqlstate: "42883".to_owned(),
            message: "operator does not exist: text = numeric".to_owned(),
            position: None,
        };
        let mut overrides = Overrides::default();
        assert!(learn(
            &failure,
            Some(1),
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
        let again = prepare(sql, &parsed, &Catalog::default(), &overrides);
        assert_eq!(again.stubs[0].signature(), "app.level() → numeric");
    }

    #[test]
    fn signatures_and_types_are_read_from_messages() {
        assert_eq!(
            function_signature("function auth.uid(uuid, unknown) does not exist"),
            Some((
                "auth.uid".to_owned(),
                vec!["uuid".to_owned(), "unknown".to_owned()]
            ))
        );
        assert_eq!(
            function_signature("function f() does not exist"),
            Some(("f".to_owned(), vec![]))
        );
        assert_eq!(
            split_types("numeric(10,2), \"My, Type\", timestamp with time zone"),
            ["numeric(10,2)", "\"My, Type\"", "timestamp with time zone"]
        );
        assert_eq!(
            wanted_type("column \"id\" is of type uuid but default expression is of type text"),
            Some("uuid".to_owned())
        );
        assert_eq!(
            wanted_type("argument of CHECK must be type boolean, not type text"),
            Some("boolean".to_owned())
        );
        assert_eq!(wanted_type("something else"), None);
    }

    #[test]
    fn a_missing_signature_corrects_the_stub_arguments() {
        let sql = "CREATE TABLE t (id uuid, CHECK (f(CASE WHEN true THEN id END)));";
        let parsed = parse(sql);
        let prepared = prepare(sql, &parsed, &Catalog::default(), &Overrides::default());
        // The inference cannot type a CASE (so guesses text); Postgres says
        // uuid.
        assert_eq!(prepared.stubs[0].signature(), "public.f(text) → boolean");
        let at = prepared.script.rfind("f(CASE").unwrap();
        let failure = Failure {
            sqlstate: "42883".to_owned(),
            message: "function f(uuid) does not exist".to_owned(),
            position: Some(at),
        };
        let mut overrides = Overrides::default();
        assert!(learn(
            &failure,
            None,
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
        let again = prepare(sql, &parsed, &Catalog::default(), &overrides);
        assert_eq!(again.stubs[0].signature(), "public.f(uuid) → boolean");
        // Learning the same thing twice changes nothing: no retry loop.
        assert!(!learn(
            &failure,
            None,
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
    }

    #[test]
    fn an_operator_mismatch_corrects_the_stub_return_type() {
        let sql = "CREATE TABLE t (n numeric);\nCREATE POLICY p ON t USING (n = (SELECT 1) OR app.level() > n::int + 0 OR app.level() = n);";
        let parsed = parse(sql);
        let prepared = prepare(sql, &parsed, &Catalog::default(), &Overrides::default());
        let operator = prepared.script.rfind("= n").unwrap();
        let failure = Failure {
            sqlstate: "42883".to_owned(),
            message: "operator does not exist: text = numeric".to_owned(),
            position: Some(operator),
        };
        let mut overrides = Overrides::default();
        assert!(learn(
            &failure,
            None,
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
        let again = prepare(sql, &parsed, &Catalog::default(), &overrides);
        assert_eq!(again.stubs[0].signature(), "app.level() → numeric");
    }

    #[test]
    fn a_type_mismatch_at_a_call_corrects_its_return_type() {
        let sql = "ALTER TABLE t ADD CONSTRAINT c CHECK (app.ok(1));";
        let parsed = parse(sql);
        let prepared = prepare(sql, &parsed, &Catalog::default(), &Overrides::default());
        let call = prepared.script.rfind("app.ok").unwrap();
        let failure = Failure {
            sqlstate: "42804".to_owned(),
            message: "argument of CHECK must be type boolean, not type text".to_owned(),
            position: Some(call),
        };
        let mut overrides = Overrides::default();
        // Already boolean: nothing to learn.
        assert!(!learn(
            &failure,
            None,
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
        let failure = Failure {
            message: "column \"c\" is of type bigint but default expression is of type boolean"
                .to_owned(),
            ..failure
        };
        assert!(learn(
            &failure,
            None,
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
    }

    #[test]
    fn message_text_that_is_not_a_lone_type_teaches_nothing() {
        let sql = "CREATE TABLE t (id uuid, c int, CHECK (f(CASE WHEN true THEN id END)), CHECK (g() = c));";
        let parsed = parse(sql);
        let prepared = prepare(sql, &parsed, &Catalog::default(), &Overrides::default());
        let at = |needle: &str| Some(prepared.script.rfind(needle).unwrap());
        for (sqlstate, message, position) in [
            (
                "42883",
                "function f(integer); DROP SCHEMA public; --) does not exist",
                at("f(CASE"),
            ),
            (
                "42883",
                "function f(int) RETURNS int AS $$ SELECT 1 $$; --) does not exist",
                at("f(CASE"),
            ),
            (
                "42883",
                "operator does not exist: text = integer; DROP SCHEMA public",
                at("= c"),
            ),
            (
                "42804",
                "column \"c\" is of type int) RETURNS int AS 'x'; -- but default expression is of type text",
                at("g()"),
            ),
            (
                "42804",
                "argument of CHECK must be type boolean; DROP TABLE t, not type text",
                at("f(CASE"),
            ),
        ] {
            let failure = Failure {
                sqlstate: sqlstate.to_owned(),
                message: message.to_owned(),
                position,
            };
            let mut overrides = Overrides::default();
            assert!(
                !learn(&failure, Some(0), &prepared, &parsed, sql, &mut overrides),
                "{message}"
            );
            assert_eq!(overrides, Overrides::default());
        }
        // A real, oddly named type is learnt, and written back quoted.
        let failure = Failure {
            sqlstate: "42883".to_owned(),
            message: "function f(\"x$$); DROP SCHEMA public; --\") does not exist".to_owned(),
            position: at("f(CASE"),
        };
        let mut overrides = Overrides::default();
        assert!(learn(
            &failure,
            Some(0),
            &prepared,
            &parsed,
            sql,
            &mut overrides
        ));
        let again = prepare(sql, &parsed, &Catalog::default(), &overrides);
        assert!(again.script.contains(
            "CREATE FUNCTION public.f(\"x$$); DROP SCHEMA public; --\") RETURNS pg_catalog.bool"
        ));
    }

    #[test]
    fn errors_about_other_functions_teach_nothing() {
        let sql = "CREATE TABLE t (x int DEFAULT lower(1));";
        let parsed = parse(sql);
        let prepared = prepare(
            sql,
            &parsed,
            &Catalog::new(["lower".to_owned()], []),
            &Overrides::default(),
        );
        let failure = Failure {
            sqlstate: "42883".to_owned(),
            message: "function lower(integer) does not exist".to_owned(),
            position: prepared.script.find("lower"),
        };
        assert!(!learn(
            &failure,
            None,
            &prepared,
            &parsed,
            sql,
            &mut Overrides::default()
        ));
    }
}
