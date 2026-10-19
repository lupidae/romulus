//! Expressions: the parse tree's raw expressions analysed as Postgres's
//! parser would (types, the operator each comparison resolves to, the
//! implicit casts that adds, constants coerced to their column's type), and
//! printed back as `ruleutils.c` prints them: `pg_get_expr` and
//! `pg_get_constraintdef` for definitions, its pretty mode for views.
//!
//! A subset, chosen for what schemas are made of: constants, columns,
//! operators, function calls, casts, `AND`/`OR`/`NOT`, `IS [NOT] NULL`,
//! `IN`, `BETWEEN`, `LIKE`, `CASE`, `COALESCE`, `NULLIF`, `GREATEST`,
//! `LEAST`, arrays, `CURRENT_TIMESTAMP` and its kin. Anything else is kept
//! as written ([`Expr::Raw`]), so every expression still prints.

use remus_core::emit::ident::{quote_ident, quote_literal};
use serde_json::Value;

use crate::{
    tree::{self, Literal},
    types::{Base, Category, Ty},
};

/// A column, by the ids that survive renames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnRef {
    pub rel: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A constant of a known type: its value as the type's output function
    /// prints it. `Null` constants print `NULL` (with a label where typed).
    Const {
        value: Option<String>,
        ty: Ty,
    },
    /// A string literal no type has claimed yet.
    Unknown {
        value: String,
    },
    Column {
        column: ColumnRef,
        ty: Ty,
    },
    /// `VALUE`, in a domain's check.
    DomainValue {
        ty: Ty,
    },
    Op {
        op: String,
        left: Option<Box<Expr>>,
        right: Box<Expr>,
        ty: Ty,
    },
    Func {
        name: String,
        /// Printed before the name: extension functions are schema-qualified.
        schema: Option<String>,
        args: Vec<Expr>,
        star: bool,
        distinct: bool,
        ty: Ty,
        /// `EXTRACT(year FROM x)` and the other SQL-syntax calls, as written.
        sql_syntax: Option<String>,
    },
    Cast {
        arg: Box<Expr>,
        ty: Ty,
        /// Implicit casts print only where Postgres shows them (inside
        /// operators and function calls).
        implicit: bool,
    },
    Bool {
        op: BoolOp,
        args: Vec<Expr>,
    },
    NullTest {
        arg: Box<Expr>,
        not: bool,
    },
    BoolTest {
        arg: Box<Expr>,
        test: &'static str,
    },
    /// `x = ANY (ARRAY[…])` (what `IN` becomes), `x <> ALL (…)`.
    AnyAll {
        op: String,
        any: bool,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Array {
        elements: Vec<Expr>,
        ty: Ty,
    },
    Coalesce {
        args: Vec<Expr>,
        ty: Ty,
    },
    NullIf {
        args: Vec<Expr>,
        ty: Ty,
    },
    MinMax {
        greatest: bool,
        args: Vec<Expr>,
        ty: Ty,
    },
    Case {
        arg: Option<Box<Expr>>,
        whens: Vec<(Expr, Expr)>,
        default: Option<Box<Expr>>,
        ty: Ty,
    },
    /// `CURRENT_TIMESTAMP`, `CURRENT_USER`…
    SqlValue {
        text: &'static str,
        ty: Ty,
    },
    Collate {
        arg: Box<Expr>,
        collation: String,
    },
    IsDistinct {
        not: bool,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// Kept as written, whitespace squashed.
    Raw {
        text: String,
        ty: Ty,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
    Not,
}

impl Expr {
    pub fn ty(&self) -> Ty {
        match self {
            Self::Const { ty, .. }
            | Self::Column { ty, .. }
            | Self::DomainValue { ty }
            | Self::Op { ty, .. }
            | Self::Func { ty, .. }
            | Self::Cast { ty, .. }
            | Self::Array { ty, .. }
            | Self::Coalesce { ty, .. }
            | Self::NullIf { ty, .. }
            | Self::MinMax { ty, .. }
            | Self::Case { ty, .. }
            | Self::SqlValue { ty, .. }
            | Self::Raw { ty, .. } => ty.clone(),
            Self::Unknown { .. } => Ty::unknown(),
            Self::Bool { .. }
            | Self::NullTest { .. }
            | Self::BoolTest { .. }
            | Self::AnyAll { .. }
            | Self::IsDistinct { .. } => Ty::builtin("bool"),
            Self::Collate { arg, .. } => arg.ty(),
        }
    }

    /// Every column the expression reads, in the order met.
    pub fn columns(&self, out: &mut Vec<ColumnRef>) {
        let mut visit = |e: &Self| e.columns(out);
        match self {
            Self::Column { column, .. } => {
                if !out.contains(column) {
                    out.push(*column);
                }
            }
            Self::Op { left, right, .. } => {
                if let Some(left) = left {
                    visit(left);
                }
                visit(right);
            }
            Self::Func { args, .. }
            | Self::Bool { args, .. }
            | Self::Coalesce { args, .. }
            | Self::NullIf { args, .. }
            | Self::MinMax { args, .. }
            | Self::Array { elements: args, .. } => args.iter().for_each(visit),
            Self::Cast { arg, .. }
            | Self::NullTest { arg, .. }
            | Self::BoolTest { arg, .. }
            | Self::Collate { arg, .. } => visit(arg),
            Self::AnyAll { left, right, .. } | Self::IsDistinct { left, right, .. } => {
                visit(left);
                visit(right);
            }
            Self::Case {
                arg,
                whens,
                default,
                ..
            } => {
                if let Some(arg) = arg {
                    visit(arg);
                }
                for (w, t) in whens {
                    visit(w);
                    visit(t);
                }
                if let Some(d) = default {
                    visit(d);
                }
            }
            Self::Const { .. }
            | Self::Unknown { .. }
            | Self::DomainValue { .. }
            | Self::SqlValue { .. }
            | Self::Raw { .. } => {}
        }
    }

    /// Every type the expression holds, to rename a type everywhere.
    pub fn types_mut(&mut self, f: &mut impl FnMut(&mut Ty)) {
        match self {
            Self::Const { ty, .. }
            | Self::Column { ty, .. }
            | Self::DomainValue { ty }
            | Self::SqlValue { ty, .. }
            | Self::Raw { ty, .. } => f(ty),
            Self::Unknown { .. } => {}
            Self::Op {
                left, right, ty, ..
            } => {
                f(ty);
                if let Some(left) = left {
                    left.types_mut(f);
                }
                right.types_mut(f);
            }
            Self::Func { args, ty, .. }
            | Self::Coalesce { args, ty }
            | Self::NullIf { args, ty }
            | Self::MinMax { args, ty, .. }
            | Self::Array {
                elements: args, ty, ..
            } => {
                f(ty);
                for a in args.iter_mut() {
                    a.types_mut(f);
                }
            }
            Self::Bool { args, .. } => args.iter_mut().for_each(|a| a.types_mut(f)),
            Self::Cast { arg, ty, .. } => {
                f(ty);
                arg.types_mut(f);
            }
            Self::NullTest { arg, .. } | Self::BoolTest { arg, .. } | Self::Collate { arg, .. } => {
                arg.types_mut(f);
            }
            Self::AnyAll { left, right, .. } | Self::IsDistinct { left, right, .. } => {
                left.types_mut(f);
                right.types_mut(f);
            }
            Self::Case {
                arg,
                whens,
                default,
                ty,
            } => {
                f(ty);
                if let Some(arg) = arg {
                    arg.types_mut(f);
                }
                for (w, t) in whens {
                    w.types_mut(f);
                    t.types_mut(f);
                }
                if let Some(d) = default {
                    d.types_mut(f);
                }
            }
        }
    }

    /// Whether the expression calls a function that is not immutable, or
    /// reads no column at all (what a generated column refuses).
    pub fn is_volatile(&self) -> bool {
        match self {
            Self::Func { name, args, .. } => {
                matches!(
                    name.as_str(),
                    "now"
                        | "random"
                        | "gen_random_uuid"
                        | "clock_timestamp"
                        | "statement_timestamp"
                        | "transaction_timestamp"
                        | "nextval"
                        | "current_setting"
                        | "uuid_generate_v4"
                        | "uuid_generate_v1"
                ) || args.iter().any(Self::is_volatile)
            }
            Self::SqlValue { .. } => true,
            Self::Op { left, right, .. } => {
                left.as_deref().is_some_and(Self::is_volatile) || right.is_volatile()
            }
            Self::Cast { arg, .. } | Self::Collate { arg, .. } => arg.is_volatile(),
            Self::Bool { args, .. } | Self::Coalesce { args, .. } => {
                args.iter().any(Self::is_volatile)
            }
            _ => false,
        }
    }
}

// ---- Analysis -------------------------------------------------------------------

/// Why an expression cannot be analysed: Postgres's error, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub sqlstate: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub location: Option<usize>,
}

impl ExprError {
    fn new(sqlstate: &'static str, message: String, location: Option<usize>) -> Self {
        Self {
            sqlstate,
            message,
            hint: None,
            location,
        }
    }

    /// This engine's own failure (see `Fail::internal`), which `Fail` turns
    /// back into one.
    pub fn internal() -> Self {
        Self::new(INTERNAL, String::new(), None)
    }
}

/// The SQLSTATE of [`ExprError::internal`].
pub const INTERNAL: &str = "XX000";

/// What names mean where the expression is: columns, and types.
pub trait Scope {
    /// The column `qualifier.name` (or `name`), its type.
    fn column(
        &self,
        qualifier: Option<&str>,
        name: &str,
        location: Option<usize>,
    ) -> std::result::Result<(ColumnRef, Ty), ExprError>;

    /// The type a `TypeName` node's fields resolve to.
    fn type_name(&self, fields: &Value) -> std::result::Result<Ty, ExprError>;

    /// The schema of an extension function the text installed, if any.
    fn extension_function(&self, name: &str) -> Option<(String, Ty)>;

    /// `VALUE` is allowed (a domain check): its type.
    fn domain_value(&self) -> Option<Ty> {
        None
    }
}

pub struct Analyzer<'a> {
    pub sql: &'a str,
    pub scope: &'a dyn Scope,
}

type Result<T> = std::result::Result<T, ExprError>;

impl Analyzer<'_> {
    /// Analyses a raw expression (an `A_Expr`, `FuncCall`…).
    pub fn expr(&self, node: &Value) -> Result<Expr> {
        let Some((kind, f)) = tree::kind(node) else {
            return Ok(self.raw(node, Ty::unknown()));
        };
        match kind {
            "A_Const" => Ok(constant(&tree::literal(f))),
            "ColumnRef" => self.column_ref(f),
            "TypeCast" => self.type_cast(f),
            "A_Expr" => self.a_expr(f, node),
            "BoolExpr" => self.bool_expr(f),
            "NullTest" => {
                let arg = self.expr(tree::field(f, "arg").unwrap_or(&Value::Null))?;
                Ok(Expr::NullTest {
                    arg: Box::new(resolve_unknown(arg, &Ty::builtin("text"))),
                    not: tree::str(f, "nulltesttype") == "IS_NOT_NULL",
                })
            }
            "BooleanTest" => {
                let arg = self.expr(tree::field(f, "arg").unwrap_or(&Value::Null))?;
                let test = match tree::str(f, "booltesttype") {
                    "IS_TRUE" => "IS TRUE",
                    "IS_NOT_TRUE" => "IS NOT TRUE",
                    "IS_FALSE" => "IS FALSE",
                    "IS_NOT_FALSE" => "IS NOT FALSE",
                    "IS_UNKNOWN" => "IS UNKNOWN",
                    _ => "IS NOT UNKNOWN",
                };
                Ok(Expr::BoolTest {
                    arg: Box::new(coerce_implicit(arg, &Ty::builtin("bool"))),
                    test,
                })
            }
            "FuncCall" => self.func_call(f, node),
            "CoalesceExpr" => {
                let args = self.exprs(tree::list(f, "args"))?;
                let ty = common_type(&args);
                Ok(Expr::Coalesce {
                    args: args.into_iter().map(|a| coerce_implicit(a, &ty)).collect(),
                    ty,
                })
            }
            "MinMaxExpr" => {
                let args = self.exprs(tree::list(f, "args"))?;
                let ty = common_type(&args);
                Ok(Expr::MinMax {
                    greatest: tree::str(f, "op") == "IS_GREATEST",
                    args: args.into_iter().map(|a| coerce_implicit(a, &ty)).collect(),
                    ty,
                })
            }
            "CaseExpr" => self.case_expr(f),
            "A_ArrayExpr" => {
                let elements = self.exprs(tree::list(f, "elements"))?;
                let element = common_type(&elements);
                Ok(Expr::Array {
                    elements: elements
                        .into_iter()
                        .map(|e| coerce_implicit(e, &element))
                        .collect(),
                    ty: element.array_of(),
                })
            }
            "SQLValueFunction" => Ok(sql_value(tree::str(f, "op"))),
            "CollateClause" => {
                let arg = self.expr(tree::field(f, "arg").unwrap_or(&Value::Null))?;
                let collation = tree::strings(tree::list(f, "collname"));
                Ok(Expr::Collate {
                    arg: Box::new(arg),
                    collation: collation
                        .iter()
                        .map(|c| quote_always(c))
                        .collect::<Vec<_>>()
                        .join("."),
                })
            }
            _ => Ok(self.raw(node, Ty::unknown())),
        }
    }

    fn exprs(&self, nodes: &[Value]) -> Result<Vec<Expr>> {
        nodes.iter().map(|n| self.expr(n)).collect()
    }

    /// The text of `node` as written, from its location to the end of what
    /// it spans (approximated by the next statement boundary it can see).
    fn raw(&self, node: &Value, ty: Ty) -> Expr {
        Expr::Raw {
            text: squash(&source_of(self.sql, node)),
            ty,
        }
    }

    fn column_ref(&self, f: &Value) -> Result<Expr> {
        let fields = tree::list(f, "fields");
        let location = tree::location(f);
        let names = tree::strings(fields);
        if names.len() != fields.len() {
            // `t.*`: only meaningful in a target list.
            return Ok(Expr::Raw {
                text: source_slice(self.sql, location, 1),
                ty: Ty::unknown(),
            });
        }
        if let [name] = names.as_slice()
            && name == "value"
            && let Some(ty) = self.scope.domain_value()
        {
            return Ok(Expr::DomainValue { ty });
        }
        let (qualifier, name) = match names.as_slice() {
            [name] => (None, name.as_str()),
            [.., qualifier, name] => (Some(qualifier.as_str()), name.as_str()),
            [] => return Ok(self.raw(&Value::Null, Ty::unknown())),
        };
        let (column, ty) = self.scope.column(qualifier, name, location)?;
        Ok(Expr::Column { column, ty })
    }

    fn type_cast(&self, f: &Value) -> Result<Expr> {
        let target = self
            .scope
            .type_name(tree::field(f, "typeName").unwrap_or(&Value::Null))?;
        let arg = self.expr(tree::field(f, "arg").unwrap_or(&Value::Null))?;
        Ok(cast_explicit(arg, &target))
    }

    fn bool_expr(&self, f: &Value) -> Result<Expr> {
        let op = match tree::str(f, "boolop") {
            "AND_EXPR" => BoolOp::And,
            "OR_EXPR" => BoolOp::Or,
            _ => BoolOp::Not,
        };
        let args = self
            .exprs(tree::list(f, "args"))?
            .into_iter()
            .map(|a| coerce_implicit(a, &Ty::builtin("bool")))
            .collect();
        Ok(Expr::Bool { op, args })
    }

    fn case_expr(&self, f: &Value) -> Result<Expr> {
        let arg = tree::field(f, "arg").map(|a| self.expr(a)).transpose()?;
        let mut whens = Vec::new();
        for w in tree::list(f, "args") {
            let Some(w) = tree::as_kind(w, "CaseWhen") else {
                continue;
            };
            let cond = self.expr(tree::field(w, "expr").unwrap_or(&Value::Null))?;
            let result = self.expr(tree::field(w, "result").unwrap_or(&Value::Null))?;
            whens.push((cond, result));
        }
        let default = tree::field(f, "defresult")
            .map(|d| self.expr(d))
            .transpose()?;
        let mut results: Vec<Expr> = whens.iter().map(|(_, r)| r.clone()).collect();
        results.extend(default.clone());
        let ty = common_type(&results);
        Ok(Expr::Case {
            arg: arg.map(Box::new),
            whens: whens
                .into_iter()
                .map(|(c, r)| {
                    (
                        coerce_implicit(c, &Ty::builtin("bool")),
                        coerce_implicit(r, &ty),
                    )
                })
                .collect(),
            default: default.map(|d| Box::new(coerce_implicit(d, &ty))),
            ty,
        })
    }

    fn a_expr(&self, f: &Value, node: &Value) -> Result<Expr> {
        let op = tree::strings(tree::list(f, "name"))
            .pop()
            .unwrap_or_default();
        let location = tree::location(f);
        let side = |name: &str| -> Result<Option<Expr>> {
            tree::field(f, name).map(|n| self.expr(n)).transpose()
        };
        match tree::str(f, "kind") {
            "AEXPR_OP" => {
                let left = side("lexpr")?;
                let right = side("rexpr")?.unwrap_or_else(|| Expr::Unknown {
                    value: String::new(),
                });
                match left {
                    Some(left) => binary(&op, left, right, location),
                    None => Ok(prefix(&op, right)),
                }
            }
            "AEXPR_LIKE" | "AEXPR_ILIKE" => {
                let like = match (tree::str(f, "kind"), op.as_str()) {
                    ("AEXPR_LIKE", "~~") => "~~",
                    ("AEXPR_LIKE", _) => "!~~",
                    (_, "~~*") => "~~*",
                    _ => "!~~*",
                };
                let left = side("lexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let right = side("rexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                binary(like, left, right, location)
            }
            "AEXPR_IN" => {
                let left = side("lexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let items = tree::field(f, "rexpr").map_or(&[][..], tree::list_items);
                let items = self.exprs(items)?;
                Self::in_list(&op, left, items, location)
            }
            "AEXPR_BETWEEN" | "AEXPR_NOT_BETWEEN" => {
                let left = side("lexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let bounds = tree::field(f, "rexpr").map_or(&[][..], tree::list_items);
                let [low, high] = self.exprs(bounds)?.try_into().map_err(|_| {
                    ExprError::new("42601", "malformed BETWEEN".to_owned(), location)
                })?;
                let between = tree::str(f, "kind") == "AEXPR_BETWEEN";
                let (a, b, joined) = if between {
                    (">=", "<=", BoolOp::And)
                } else {
                    ("<", ">", BoolOp::Or)
                };
                Ok(Expr::Bool {
                    op: joined,
                    args: vec![
                        binary(a, left.clone(), low, location)?,
                        binary(b, left, high, location)?,
                    ],
                })
            }
            "AEXPR_DISTINCT" | "AEXPR_NOT_DISTINCT" => {
                let left = side("lexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let right = side("rexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let ty = comparison_type(&left, &right);
                Ok(Expr::IsDistinct {
                    not: tree::str(f, "kind") == "AEXPR_NOT_DISTINCT",
                    left: Box::new(coerce_operand(left, &ty)),
                    right: Box::new(coerce_operand(right, &ty)),
                })
            }
            "AEXPR_NULLIF" => {
                let left = side("lexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let right = side("rexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let ty = left.ty();
                Ok(Expr::NullIf {
                    args: vec![left, resolve_unknown(right, &ty)],
                    ty,
                })
            }
            "AEXPR_OP_ANY" | "AEXPR_OP_ALL" => {
                let left = side("lexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let right = side("rexpr")?.unwrap_or_else(|| self.raw(node, Ty::unknown()));
                let element = right.ty().element();
                let ty = comparison_type(
                    &left,
                    &Expr::Raw {
                        text: String::new(),
                        ty: element,
                    },
                );
                Ok(Expr::AnyAll {
                    op,
                    any: tree::str(f, "kind") == "AEXPR_OP_ANY",
                    left: Box::new(coerce_operand(left, &ty)),
                    right: Box::new(resolve_unknown(right, &ty.array_of())),
                })
            }
            _ => Ok(self.raw(node, Ty::builtin("bool"))),
        }
    }

    /// `x IN (a, b)`: `x = ANY (ARRAY[a, b])` when the list's items share a
    /// type, else `x = a OR x = b` (as `transformAExprIn` decides).
    fn in_list(op: &str, left: Expr, items: Vec<Expr>, location: Option<usize>) -> Result<Expr> {
        let negated = op == "<>";
        if let [item] = items.as_slice() {
            return binary(op, left, item.clone(), location);
        }
        let all: Vec<Expr> = std::iter::once(left.clone())
            .chain(items.iter().cloned())
            .collect();
        let ty = common_type(&all);
        // varchar: Postgres compares as text, the array cast as a whole.
        let compare = if ty.category() == Category::String && !ty.is("bpchar") {
            Ty::builtin("text")
        } else {
            ty.plain()
        };
        let elements: Vec<Expr> = items
            .into_iter()
            .map(|e| coerce_implicit(e, &ty.plain()))
            .collect();
        let mut array = Expr::Array {
            elements,
            ty: ty.plain().array_of(),
        };
        if compare != ty.plain() {
            array = Expr::Cast {
                arg: Box::new(array),
                ty: compare.array_of(),
                implicit: true,
            };
        }
        Ok(Expr::AnyAll {
            op: op.to_owned(),
            any: !negated,
            left: Box::new(coerce_implicit(left, &compare)),
            right: Box::new(array),
        })
    }

    fn func_call(&self, f: &Value, node: &Value) -> Result<Expr> {
        let names = tree::strings(tree::list(f, "funcname"));
        let Some(name) = names.last().cloned() else {
            return Ok(self.raw(node, Ty::unknown()));
        };
        let qualifier = match names.as_slice() {
            [.., q, _] => Some(q.clone()),
            _ => None,
        };
        if tree::field(f, "over").is_some() || tree::field(f, "agg_filter").is_some() {
            return Ok(self.raw(node, Ty::unknown()));
        }
        let args = self.exprs(tree::list(f, "args"))?;
        let star = tree::bool(f, "agg_star");
        let distinct = tree::bool(f, "agg_distinct");
        let sql_syntax = tree::str(f, "funcformat") == "COERCE_SQL_SYNTAX";
        if sql_syntax {
            return Ok(self.sql_syntax(&name, args, node));
        }
        let builtin = qualifier.as_deref().is_none_or(|q| q == "pg_catalog");
        let (schema, ty, args) =
            if let Some((ret, params)) = builtin.then(|| signature(&name, &args)).flatten() {
                let args = args
                    .into_iter()
                    .zip(params.into_iter().chain(std::iter::repeat(None)))
                    .map(|(a, p)| match p {
                        Some(p) => coerce_implicit(a, &p),
                        None => resolve_unknown(a, &Ty::builtin("text")),
                    })
                    .collect();
                (None, ret, args)
            } else if let Some((schema, ret)) = self.scope.extension_function(&name) {
                let args = args
                    .into_iter()
                    .map(|a| resolve_unknown(a, &Ty::builtin("text")))
                    .collect();
                (Some(schema), ret, args)
            } else {
                // The app's sandbox stubs a function the text calls without
                // defining it, where Postgres would look: the path's first schema.
                let args = args
                    .into_iter()
                    .map(|a| resolve_unknown(a, &Ty::builtin("text")))
                    .collect();
                (
                    Some(qualifier.unwrap_or_else(|| "public".to_owned())),
                    Ty::unknown(),
                    args,
                )
            };
        Ok(Expr::Func {
            name,
            schema,
            args,
            star,
            distinct,
            ty,
            sql_syntax: None,
        })
    }

    /// `EXTRACT(year FROM x)`, `TRIM(BOTH FROM x)`, `SUBSTRING(…)`…: printed
    /// as the SQL syntax they were written in.
    fn sql_syntax(&self, name: &str, args: Vec<Expr>, node: &Value) -> Expr {
        match (name, args.as_slice()) {
            ("extract", [field, source]) => {
                let field = match field {
                    Expr::Unknown { value }
                    | Expr::Const {
                        value: Some(value), ..
                    } => value.clone(),
                    _ => String::from("?"),
                };
                Expr::Func {
                    name: name.to_owned(),
                    schema: None,
                    args: vec![source.clone()],
                    star: false,
                    distinct: false,
                    ty: Ty::builtin("numeric"),
                    sql_syntax: Some(field),
                }
            }
            ("btrim" | "ltrim" | "rtrim", [_, ..]) => Expr::Func {
                name: name.to_owned(),
                schema: None,
                args: args
                    .into_iter()
                    .map(|a| coerce_implicit(a, &Ty::builtin("text")))
                    .collect(),
                star: false,
                distinct: false,
                ty: Ty::builtin("text"),
                sql_syntax: None,
            },
            _ => self.raw(node, Ty::unknown()),
        }
    }
}

/// A literal as the grammar types it: integers that fit `int4` are `int4`,
/// larger ones `int8`, numbers with a point `numeric`, strings unknown.
pub fn constant(literal: &Literal) -> Expr {
    match literal {
        Literal::Integer(i) if i32::try_from(*i).is_ok() => Expr::Const {
            value: Some(i.to_string()),
            ty: Ty::builtin("int4"),
        },
        Literal::Integer(i) => Expr::Const {
            value: Some(i.to_string()),
            ty: Ty::builtin("int8"),
        },
        Literal::Float(text) => {
            let integral = text
                .trim_start_matches('-')
                .bytes()
                .all(|b| b.is_ascii_digit());
            Expr::Const {
                value: Some(normalize_numeric(text)),
                ty: Ty::builtin(if integral && text.len() < 19 {
                    "int8"
                } else {
                    "numeric"
                }),
            }
        }
        Literal::String(s) => Expr::Unknown { value: s.clone() },
        Literal::Bool(b) => Expr::Const {
            value: Some(b.to_string()),
            ty: Ty::builtin("bool"),
        },
        Literal::BitString(s) => Expr::Const {
            value: Some(s.trim_start_matches(['b', 'B']).to_owned()),
            ty: Ty::builtin("bit"),
        },
        Literal::Null => Expr::Const {
            value: None,
            ty: Ty::unknown(),
        },
    }
}

/// `1.50` stays `1.50` (numeric keeps its scale), `.5` becomes `0.5`,
/// `1e3` becomes `1000`.
fn normalize_numeric(text: &str) -> String {
    let (sign, digits) = text
        .strip_prefix('-')
        .map_or(("", text), |rest| ("-", rest));
    // Past numeric's exponent limit (NUMERIC_MAX_PRECISION) Postgres refuses
    // the value; the text is kept rather than spelled out.
    if let Some((mantissa, exponent)) = digits.split_once(['e', 'E'])
        && let Ok(exponent) = exponent.parse::<i32>()
        && exponent.abs() <= 1000
    {
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let mut all: String = format!("{int}{frac}");
        let mut point = i32::try_from(int.len()).unwrap_or(0) + exponent;
        if point <= 0 {
            all = "0".repeat(usize::try_from(-point).unwrap_or(0) + 1) + &all;
            point = 1;
        }
        let point = usize::try_from(point).unwrap_or(0);
        while all.len() < point {
            all.push('0');
        }
        let (i, f) = all.split_at_checked(point).unwrap_or((&all, ""));
        let i = i.trim_start_matches('0');
        let i = if i.is_empty() { "0" } else { i };
        return if f.is_empty() {
            format!("{sign}{i}")
        } else {
            format!("{sign}{i}.{f}")
        };
    }
    if digits.starts_with('.') {
        return format!("{sign}0{digits}");
    }
    text.to_owned()
}

fn sql_value(op: &str) -> Expr {
    let (text, ty) = match op {
        "SVFOP_CURRENT_DATE" => ("CURRENT_DATE", "date"),
        "SVFOP_CURRENT_TIME" | "SVFOP_CURRENT_TIME_N" => ("CURRENT_TIME", "timetz"),
        "SVFOP_CURRENT_TIMESTAMP" | "SVFOP_CURRENT_TIMESTAMP_N" => {
            ("CURRENT_TIMESTAMP", "timestamptz")
        }
        "SVFOP_LOCALTIME" | "SVFOP_LOCALTIME_N" => ("LOCALTIME", "time"),
        "SVFOP_LOCALTIMESTAMP" | "SVFOP_LOCALTIMESTAMP_N" => ("LOCALTIMESTAMP", "timestamp"),
        "SVFOP_CURRENT_ROLE" => ("CURRENT_ROLE", "name"),
        "SVFOP_CURRENT_USER" => ("CURRENT_USER", "name"),
        "SVFOP_USER" => ("USER", "name"),
        "SVFOP_SESSION_USER" => ("SESSION_USER", "name"),
        "SVFOP_CURRENT_CATALOG" => ("CURRENT_CATALOG", "name"),
        _ => ("CURRENT_SCHEMA", "name"),
    };
    Expr::SqlValue {
        text,
        ty: Ty::builtin(ty),
    }
}

/// The return type and parameter types of the built-in functions schemas
/// use, `None` for a parameter that takes any type.
fn signature(name: &str, args: &[Expr]) -> Option<(Ty, Vec<Option<Ty>>)> {
    let t = |n: &'static str| Some(Ty::builtin(n));
    let first = args.first().map_or_else(Ty::unknown, Expr::ty);
    let numeric_arg = || {
        if first.category() == Category::Numeric {
            first.plain()
        } else {
            Ty::builtin("numeric")
        }
    };
    Some(match (name, args.len()) {
        ("now" | "statement_timestamp" | "transaction_timestamp" | "clock_timestamp", 0) => {
            (Ty::builtin("timestamptz"), vec![])
        }
        ("gen_random_uuid" | "uuidv7" | "uuidv4", 0) => (Ty::builtin("uuid"), vec![]),
        ("random", 0) => (Ty::builtin("float8"), vec![]),
        (
            "lower" | "upper" | "initcap" | "reverse" | "md5" | "quote_ident" | "quote_literal",
            1,
        ) => (Ty::builtin("text"), vec![t("text")]),
        ("btrim" | "ltrim" | "rtrim", 1 | 2) | ("string_agg", 2) => {
            (Ty::builtin("text"), vec![t("text"), t("text")])
        }
        ("length" | "char_length" | "character_length" | "octet_length" | "bit_length", 1) => {
            if first.is("bytea") {
                (Ty::builtin("int4"), vec![t("bytea")])
            } else {
                (Ty::builtin("int4"), vec![t("text")])
            }
        }
        ("left" | "right" | "repeat", 2) => (Ty::builtin("text"), vec![t("text"), t("int4")]),
        ("substr", 2 | 3) => (Ty::builtin("text"), vec![t("text"), t("int4"), t("int4")]),
        ("replace" | "translate", 3) => {
            (Ty::builtin("text"), vec![t("text"), t("text"), t("text")])
        }
        ("split_part", 3) => (Ty::builtin("text"), vec![t("text"), t("text"), t("int4")]),
        ("concat" | "concat_ws" | "format", _) => (Ty::builtin("text"), vec![]),
        ("to_char", 2) => (Ty::builtin("text"), vec![None, t("text")]),
        ("current_setting", 1 | 2) => (Ty::builtin("text"), vec![t("text"), t("bool")]),
        ("to_tsvector" | "to_tsquery" | "plainto_tsquery" | "websearch_to_tsquery", 1) => (
            Ty::builtin(if name == "to_tsvector" {
                "tsvector"
            } else {
                "tsquery"
            }),
            vec![t("text")],
        ),
        ("to_tsvector" | "to_tsquery" | "plainto_tsquery" | "websearch_to_tsquery", 2) => (
            Ty::builtin(if name == "to_tsvector" {
                "tsvector"
            } else {
                "tsquery"
            }),
            vec![t("regconfig"), t("text")],
        ),
        ("jsonb_build_object" | "jsonb_build_array", _) => (Ty::builtin("jsonb"), vec![]),
        ("json_build_object" | "json_build_array", _) => (Ty::builtin("json"), vec![]),
        ("jsonb_typeof" | "jsonb_pretty", 1) => (Ty::builtin("text"), vec![t("jsonb")]),
        ("jsonb_array_length", 1) => (Ty::builtin("int4"), vec![t("jsonb")]),
        ("array_length", 2) => (Ty::builtin("int4"), vec![None, t("int4")]),
        ("cardinality", 1) => (Ty::builtin("int4"), vec![None]),
        ("nextval", 1) => (Ty::builtin("int8"), vec![t("regclass")]),
        ("count", _) => (Ty::builtin("int8"), vec![None]),
        ("sum", 1) => (
            match &first.base {
                Base::Builtin("int2" | "int4") => Ty::builtin("int8"),
                Base::Builtin("float4" | "float8" | "money" | "interval") => first.plain(),
                _ => Ty::builtin("numeric"),
            },
            vec![None],
        ),
        ("avg", 1) => (
            if matches!(first.base, Base::Builtin("float4" | "float8")) {
                Ty::builtin("float8")
            } else {
                Ty::builtin("numeric")
            },
            vec![None],
        ),
        ("min" | "max", 1) => (first.plain(), vec![None]),
        ("abs" | "ceil" | "ceiling" | "floor" | "sign", 1) => {
            (numeric_arg(), vec![Some(numeric_arg())])
        }
        ("round" | "trunc", 1) => (numeric_arg(), vec![Some(numeric_arg())]),
        ("round" | "trunc", 2) => (Ty::builtin("numeric"), vec![t("numeric"), t("int4")]),
        ("date_trunc", 2) => (
            if first.is_unknown() {
                args.get(1)
                    .map_or_else(|| Ty::builtin("timestamptz"), Expr::ty)
            } else {
                first.plain()
            },
            vec![t("text"), None],
        ),
        ("date_part", 2) => (Ty::builtin("float8"), vec![t("text"), None]),
        ("age", 1 | 2) => (Ty::builtin("interval"), vec![]),
        ("array_agg", 1) => (first.plain().array_of(), vec![None]),
        ("bool_and" | "bool_or" | "every", 1) => (Ty::builtin("bool"), vec![t("bool")]),
        _ => return None,
    })
}

/// A string literal takes the type it is compared or assigned to.
pub fn resolve_unknown(expr: Expr, ty: &Ty) -> Expr {
    match expr {
        Expr::Unknown { value } => {
            let target = if ty.is_unknown() {
                Ty::builtin("text")
            } else {
                ty.clone()
            };
            typed_constant(value, &target)
        }
        Expr::Const { value: None, ty: t } if t.is_unknown() => Expr::Const {
            value: None,
            ty: ty.plain(),
        },
        other => other,
    }
}

/// A string literal read by `ty`'s input function: the value as its output
/// function would print it, for the types where that is easy to know.
fn typed_constant(value: String, ty: &Ty) -> Expr {
    let plain = ty.plain();
    let value = match &plain.base {
        Base::Builtin("bool") if !plain.array => match value.trim().to_ascii_lowercase().as_str() {
            "t" | "true" | "yes" | "y" | "on" | "1" => "true".to_owned(),
            "f" | "false" | "no" | "n" | "off" | "0" => "false".to_owned(),
            _ => value,
        },
        Base::Builtin("int2" | "int4" | "int8") if !plain.array => {
            value.trim().parse::<i64>().map_or(value, |n| n.to_string())
        }
        Base::Builtin("timestamptz") if !plain.array => normalize_timestamp(&value, true),
        Base::Builtin("timestamp") if !plain.array => normalize_timestamp(&value, false),
        _ => value,
    };
    Expr::Const {
        value: Some(value),
        ty: plain,
    }
}

/// `'2024-01-01'` as a `timestamptz` prints `2024-01-01 00:00:00+00` (the
/// sandbox runs in UTC); other spellings are kept.
fn normalize_timestamp(value: &str, zoned: bool) -> String {
    let v = value.trim();
    let is_date = v.len() == 10
        && v.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if is_date {
        return if zoned {
            format!("{v} 00:00:00+00")
        } else {
            format!("{v} 00:00:00")
        };
    }
    value.to_owned()
}

/// An explicit cast (`x::t`, `CAST(x AS t)`).
pub fn cast_explicit(arg: Expr, target: &Ty) -> Expr {
    match arg {
        // A literal read as the type: one constant, printed with its label.
        Expr::Unknown { value } => typed_constant(value, target),
        Expr::Const { value: None, .. } => Expr::Const {
            value: None,
            ty: target.plain(),
        },
        other if other.ty() == *target => other,
        other => Expr::Cast {
            arg: Box::new(other),
            ty: target.clone(),
            implicit: false,
        },
    }
}

/// Coerces `expr` to `ty` where Postgres would do it silently (operator and
/// function arguments, `CASE` branches): an implicit cast, shown only where
/// `ruleutils` shows implicit casts.
pub fn coerce_implicit(expr: Expr, ty: &Ty) -> Expr {
    let expr = resolve_unknown(expr, ty);
    let from = expr.ty();
    if ty.is_unknown() || from.is_unknown() || from.plain() == ty.plain() {
        return expr;
    }
    if matches!(expr, Expr::Const { value: None, .. }) {
        return expr;
    }
    Expr::Cast {
        arg: Box::new(expr),
        ty: ty.plain(),
        implicit: true,
    }
}

/// An operand of a comparison: coerced unless an operator takes its own
/// type as it is (integers of any width, timestamps and dates).
fn coerce_operand(expr: Expr, ty: &Ty) -> Expr {
    let from = expr.ty();
    if cross_type_ok(&from, ty) {
        return resolve_unknown(expr, ty);
    }
    coerce_implicit(expr, ty)
}

/// Pairs with an operator of their own for both types (`int84gt`,
/// `timestamptz_lt_date`…): no cast needed.
fn cross_type_ok(a: &Ty, b: &Ty) -> bool {
    let ints = ["int2", "int4", "int8"];
    let floats = ["float4", "float8"];
    let times = ["date", "timestamp", "timestamptz"];
    let named =
        |t: &Ty, set: &[&str]| matches!(t.base, Base::Builtin(n) if set.contains(&n)) && !t.array;
    (named(a, &ints) && named(b, &ints))
        || (named(a, &floats) && named(b, &floats))
        || (named(a, &times) && named(b, &times))
        || a.plain() == b.plain()
}

/// The numeric types, from the one others widen to last.
const NUMERIC_RANK: &[&str] = &["int2", "int4", "int8", "numeric", "float4", "float8"];

fn numeric_rank(ty: &Ty) -> Option<usize> {
    match ty.base {
        Base::Builtin(n) if !ty.array => NUMERIC_RANK.iter().position(|r| *r == n),
        _ => None,
    }
}

/// The wider of two numeric ranks, as a type.
fn wider(a: usize, b: usize) -> Ty {
    Ty::builtin(NUMERIC_RANK.get(a.max(b)).copied().unwrap_or("numeric"))
}

/// The type both sides of a comparison are compared as.
fn comparison_type(left: &Expr, right: &Expr) -> Ty {
    let (l, r) = (left.ty(), right.ty());
    if l.is_unknown() {
        return if r.is_unknown() {
            Ty::builtin("text")
        } else {
            r.plain()
        };
    }
    if r.is_unknown() {
        return l.plain();
    }
    if let (Some(a), Some(b)) = (numeric_rank(&l), numeric_rank(&r)) {
        return wider(a, b);
    }
    if l.category() == Category::String
        && r.category() == Category::String
        && l.plain() != r.plain()
    {
        return Ty::builtin("text");
    }
    if l.is("varchar") && r.is("varchar") {
        return Ty::builtin("text");
    }
    l.plain()
}

/// The common type of a list (a `CASE`, `COALESCE`, `IN` list, array).
pub fn common_type(exprs: &[Expr]) -> Ty {
    let mut best: Option<Ty> = None;
    for e in exprs {
        let ty = e.ty();
        if ty.is_unknown() {
            continue;
        }
        best = Some(match best {
            None => ty.plain(),
            Some(b) => match (numeric_rank(&b), numeric_rank(&ty)) {
                (Some(x), Some(y)) => wider(x, y),
                _ if b.category() == Category::String
                    && ty.category() == Category::String
                    && b != ty.plain() =>
                {
                    Ty::builtin("text")
                }
                _ => b,
            },
        });
    }
    best.unwrap_or_else(|| Ty::builtin("text"))
}

fn is_comparison(op: &str) -> bool {
    matches!(op, "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=")
}

/// A binary operator, resolved as Postgres resolves it for the types
/// schemas use; others keep their operands' types.
#[allow(clippy::too_many_lines)] // one arm per operator family
fn binary(op: &str, left: Expr, right: Expr, location: Option<usize>) -> Result<Expr> {
    let op = if op == "!=" { "<>" } else { op };
    let (l, r) = (left.ty(), right.ty());
    if is_comparison(op) {
        let ty = comparison_type(&left, &right);
        let (lt, rt) = (
            resolve_unknown(left.clone(), &ty).ty(),
            resolve_unknown(right.clone(), &ty).ty(),
        );
        if !comparable(&lt, &rt) {
            return Err(no_operator(op, &lt, &rt, location));
        }
        return Ok(Expr::Op {
            op: op.to_owned(),
            left: Some(Box::new(coerce_operand(left, &ty))),
            right: Box::new(coerce_operand(right, &ty)),
            ty: Ty::builtin("bool"),
        });
    }
    match op {
        "+" | "-" | "*" | "/" | "%" | "^" => arithmetic(op, left, right, location),
        "||" => {
            if l.array || r.array {
                let ty = if l.array { l.clone() } else { r.clone() };
                return Ok(op_expr(
                    op,
                    resolve_unknown(left, &ty),
                    resolve_unknown(right, &ty),
                    ty,
                ));
            }
            if l.is("jsonb") || r.is("jsonb") {
                let ty = Ty::builtin("jsonb");
                return Ok(op_expr(
                    op,
                    resolve_unknown(left, &ty),
                    resolve_unknown(right, &ty),
                    ty,
                ));
            }
            let text = Ty::builtin("text");
            let string = |t: &Ty| t.category() == Category::String || t.is_unknown();
            if string(&l) && string(&r) {
                Ok(op_expr(
                    op,
                    coerce_implicit(left, &text),
                    coerce_implicit(right, &text),
                    text,
                ))
            } else {
                // `anynonarray || text`: the other side is printed as it is.
                Ok(op_expr(
                    op,
                    resolve_unknown(left, &text),
                    resolve_unknown(right, &text),
                    text,
                ))
            }
        }
        "~~" | "!~~" | "~~*" | "!~~*" | "~" | "!~" | "~*" | "!~*" => {
            let ty = if l.is("bpchar") || l.is_unknown() || l.category() != Category::String {
                if l.is("bpchar") {
                    Ty::builtin("bpchar")
                } else {
                    Ty::builtin("text")
                }
            } else if l.is("text") || l.is("varchar") || l.is("name") {
                Ty::builtin("text")
            } else {
                l.plain()
            };
            let right_ty = if ty.is("bpchar") {
                Ty::builtin("text")
            } else {
                ty.clone()
            };
            Ok(op_expr(
                op,
                coerce_implicit(left, &ty),
                coerce_implicit(right, &right_ty),
                Ty::builtin("bool"),
            ))
        }
        "->" | "->>" | "#>" | "#>>" => {
            let key = if matches!(right, Expr::Const { ref ty, .. } if ty.is("int4")) {
                Ty::builtin("int4")
            } else if op.starts_with('#') {
                Ty::builtin("text").array_of()
            } else {
                Ty::builtin("text")
            };
            let json = if l.is("json") { "json" } else { "jsonb" };
            let ty = if op.ends_with(">>") {
                Ty::builtin("text")
            } else {
                Ty::builtin(json)
            };
            Ok(op_expr(
                op,
                resolve_unknown(left, &Ty::builtin(json)),
                resolve_unknown(right, &key),
                ty,
            ))
        }
        "?" | "?|" | "?&" => Ok(op_expr(
            op,
            left,
            resolve_unknown(
                right,
                &if op == "?" {
                    Ty::builtin("text")
                } else {
                    Ty::builtin("text").array_of()
                },
            ),
            Ty::builtin("bool"),
        )),
        "@>" | "<@" | "&&" => {
            let ty = if l.is_unknown() { r.plain() } else { l.plain() };
            Ok(op_expr(
                op,
                resolve_unknown(left, &ty),
                resolve_unknown(right, &ty),
                Ty::builtin("bool"),
            ))
        }
        "@@" => Ok(op_expr(
            op,
            resolve_unknown(left, &Ty::builtin("tsvector")),
            resolve_unknown(right, &Ty::builtin("tsquery")),
            Ty::builtin("bool"),
        )),
        _ => {
            let ty = if l.is_unknown() { r.plain() } else { l.plain() };
            Ok(op_expr(
                op,
                resolve_unknown(left, &ty),
                resolve_unknown(right, &ty),
                Ty::unknown(),
            ))
        }
    }
}

fn op_expr(op: &str, left: Expr, right: Expr, ty: Ty) -> Expr {
    Expr::Op {
        op: op.to_owned(),
        left: Some(Box::new(left)),
        right: Box::new(right),
        ty,
    }
}

fn arithmetic(op: &str, left: Expr, right: Expr, location: Option<usize>) -> Result<Expr> {
    let (l, r) = (left.ty(), right.ty());
    let named = |t: &Ty, n: &str| t.is(n);
    let time = |t: &Ty| t.is("timestamptz") || t.is("timestamp") || t.is("date") || t.is("time");
    // Dates and times: `+ interval`, `- interval`, `date + integer`, differences.
    if time(&l) || named(&l, "interval") || time(&r) || named(&r, "interval") {
        let (left, right) = if l.is_unknown() {
            (resolve_unknown(left, &Ty::builtin("interval")), right)
        } else {
            (
                left,
                resolve_unknown(
                    right,
                    &if time(&l) && (op == "-") && r.is_unknown() {
                        Ty::builtin("interval")
                    } else if time(&l) || named(&l, "interval") {
                        if named(&l, "date") && r.category() == Category::Numeric {
                            Ty::builtin("int4")
                        } else {
                            Ty::builtin("interval")
                        }
                    } else {
                        l.plain()
                    },
                ),
            )
        };
        let (lt, rt) = (left.ty(), right.ty());
        let ty = match (op, &lt.base, &rt.base) {
            ("-", Base::Builtin("date"), Base::Builtin("date")) => Ty::builtin("int4"),
            (_, Base::Builtin("date"), Base::Builtin("interval")) => Ty::builtin("timestamp"),
            (_, Base::Builtin("date"), Base::Builtin("int4")) => Ty::builtin("date"),
            (_, Base::Builtin("interval"), Base::Builtin("timestamptz" | "timestamp" | "date")) => {
                rt.plain()
            }
            (
                "-",
                Base::Builtin("timestamptz" | "timestamp"),
                Base::Builtin("timestamptz" | "timestamp"),
            )
            | (_, Base::Builtin("interval"), _) => Ty::builtin("interval"),
            _ => lt.plain(),
        };
        return Ok(op_expr(op, left, right, ty));
    }
    let ty = comparison_type(&left, &right);
    if numeric_rank(&ty).is_none() && !ty.is_unknown() && !matches!(ty.base, Base::User { .. }) {
        let (lt, rt) = (
            resolve_unknown(left.clone(), &ty).ty(),
            resolve_unknown(right.clone(), &ty).ty(),
        );
        if lt.category() != Category::Numeric || rt.category() != Category::Numeric {
            return Err(no_operator(op, &lt, &rt, location));
        }
    }
    // Integer arithmetic keeps each width (`int84mul`), the result the wider.
    let ints = ["int2", "int4", "int8"];
    let both_ints = [&l, &r]
        .iter()
        .all(|t| matches!(t.base, Base::Builtin(n) if ints.contains(&n)) && !t.array);
    if both_ints {
        return Ok(op_expr(op, left, right, ty));
    }
    Ok(op_expr(
        op,
        coerce_operand(left, &ty),
        coerce_operand(right, &ty),
        ty,
    ))
}

fn prefix(op: &str, right: Expr) -> Expr {
    // A negative literal is one constant, as the grammar makes it.
    if op == "-"
        && let Expr::Const { value: Some(v), ty } = &right
        && numeric_rank(ty).is_some()
    {
        return Expr::Const {
            value: Some(format!("-{v}")),
            ty: ty.clone(),
        };
    }
    let ty = right.ty();
    Expr::Op {
        op: op.to_owned(),
        left: None,
        right: Box::new(right),
        ty,
    }
}

/// Whether Postgres has a comparison between the two types.
fn comparable(a: &Ty, b: &Ty) -> bool {
    if a.is_unknown() || b.is_unknown() || a.plain() == b.plain() {
        return true;
    }
    let (ca, cb) = (a.category(), b.category());
    if matches!(a.base, Base::User { .. }) || matches!(b.base, Base::User { .. }) {
        // Extension types compare with text and themselves (citext); be lenient.
        return !(a.array ^ b.array);
    }
    if a.array && b.array {
        return comparable(&a.element(), &b.element());
    }
    if a.array != b.array {
        return false;
    }
    ca == cb && ca != Category::Other
}

fn no_operator(op: &str, l: &Ty, r: &Ty, location: Option<usize>) -> ExprError {
    ExprError {
        hint: Some(
            "No operator matches the given name and argument types. You might need to add explicit type casts."
                .to_owned(),
        ),
        ..ExprError::new(
            "42883",
            format!("operator does not exist: {} {op} {}", l.display(), r.display()),
            location,
        )
    }
}

/// Whether a value of `from` can be stored in a column of `to` (an
/// assignment cast exists), for the clear mismatches only.
pub fn assignable(from: &Ty, to: &Ty) -> bool {
    if from.is_unknown() || to.is_unknown() || from.plain() == to.plain() {
        return true;
    }
    if matches!(from.base, Base::User { .. }) || matches!(to.base, Base::User { .. }) {
        return true;
    }
    if from.array != to.array {
        return false;
    }
    let (cf, ct) = (from.category(), to.category());
    // Anything converts to a string type in an assignment (I/O conversion).
    if ct == Category::String {
        return true;
    }
    if cf == ct && cf != Category::Other {
        return true;
    }
    matches!(
        (&from.base, &to.base),
        (Base::Builtin("json"), Base::Builtin("jsonb"))
            | (Base::Builtin("jsonb"), Base::Builtin("json"))
    )
}

/// Coerces a value to a column's type for storage (a default): constants
/// take the type, other values get the implicit cast (hidden when printed).
pub fn coerce_assignment(expr: Expr, ty: &Ty) -> Expr {
    match expr {
        Expr::Unknown { .. } => resolve_unknown(expr, ty),
        Expr::Const { value: None, .. } => expr,
        Expr::Array { elements, .. } if elements.is_empty() => Expr::Array {
            elements,
            ty: ty.plain(),
        },
        other => {
            let from = other.ty();
            if from.plain() == ty.plain() || from.is_unknown() {
                other
            } else {
                Expr::Cast {
                    arg: Box::new(other),
                    ty: ty.plain(),
                    implicit: true,
                }
            }
        }
    }
}

// ---- Printing --------------------------------------------------------------------

/// How column references print: bare in a table's own expressions,
/// qualified by their relation's name (or alias) in a view of several.
pub trait Names {
    fn column(&self, column: ColumnRef) -> String;
}

pub struct Printer<'a> {
    pub names: &'a dyn Names,
    /// `pg_get_viewdef(…, true)`: parentheses only where needed.
    pub pretty: bool,
}

impl Printer<'_> {
    /// A top-level expression: a default, a policy's `USING`, a check's
    /// body (implicit casts hidden at this level).
    pub fn print(&self, expr: &Expr) -> String {
        let mut out = String::new();
        self.rule(expr, false, &mut out, None);
        out
    }

    #[allow(clippy::too_many_lines)] // one arm per node, as ruleutils' get_rule_expr
    fn rule(&self, expr: &Expr, show_implicit: bool, out: &mut String, parent: Option<&Expr>) {
        match expr {
            Expr::Const { value, ty } => const_expr(value.as_deref(), ty, out),
            Expr::Unknown { value } => out.push_str(&quote_literal(value)),
            Expr::Column { column, .. } => out.push_str(&self.names.column(*column)),
            Expr::DomainValue { .. } => out.push_str("VALUE"),
            Expr::Op {
                op, left, right, ..
            } => {
                let paren = self.need_paren(expr, parent);
                if paren {
                    out.push('(');
                }
                if let Some(left) = left {
                    self.rule(left, true, out, Some(expr));
                    out.push(' ');
                    out.push_str(op);
                    out.push(' ');
                } else {
                    out.push_str(op);
                    out.push(' ');
                }
                self.rule(right, true, out, Some(expr));
                if paren {
                    out.push(')');
                }
            }
            Expr::Func {
                name,
                schema,
                args,
                star,
                distinct,
                sql_syntax,
                ..
            } => {
                if let Some(field) = sql_syntax {
                    out.push_str("EXTRACT(");
                    out.push_str(field);
                    out.push_str(" FROM ");
                    if let Some(arg) = args.first() {
                        self.rule(arg, true, out, Some(expr));
                    }
                    out.push(')');
                    return;
                }
                if let Some(schema) = schema {
                    out.push_str(&quote_ident(schema));
                    out.push('.');
                }
                out.push_str(&quote_ident(name));
                out.push('(');
                if *star {
                    out.push('*');
                }
                if *distinct {
                    out.push_str("DISTINCT ");
                }
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.rule(arg, true, out, None);
                }
                out.push(')');
            }
            Expr::Cast { arg, ty, implicit } => {
                if *implicit && !show_implicit {
                    self.rule(arg, false, out, parent);
                    return;
                }
                // A literal under a cast to its own type prints undecorated.
                if let Expr::Const { value, ty: t } = arg.as_ref()
                    && t.plain() == ty.plain()
                {
                    match value {
                        Some(v) => out.push_str(&quote_literal(v)),
                        None => out.push_str("NULL"),
                    }
                } else if self.pretty && simple(arg) {
                    self.rule(arg, false, out, Some(expr));
                } else {
                    out.push('(');
                    self.rule(arg, false, out, Some(expr));
                    out.push(')');
                }
                out.push_str("::");
                out.push_str(&ty.display());
            }
            Expr::Bool { op, args } => {
                let paren = self.need_paren(expr, parent);
                if paren {
                    out.push('(');
                }
                match op {
                    BoolOp::Not => {
                        out.push_str("NOT ");
                        if let Some(arg) = args.first() {
                            self.rule(arg, false, out, Some(expr));
                        }
                    }
                    BoolOp::And | BoolOp::Or => {
                        let word = if *op == BoolOp::And { " AND " } else { " OR " };
                        for (i, arg) in args.iter().enumerate() {
                            if i > 0 {
                                out.push_str(word);
                            }
                            self.rule(arg, false, out, Some(expr));
                        }
                    }
                }
                if paren {
                    out.push(')');
                }
            }
            Expr::NullTest { arg, not } => {
                let paren = self.need_paren(expr, parent);
                if paren {
                    out.push('(');
                }
                self.rule(arg, true, out, Some(expr));
                out.push_str(if *not { " IS NOT NULL" } else { " IS NULL" });
                if paren {
                    out.push(')');
                }
            }
            Expr::BoolTest { arg, test } => {
                let paren = self.need_paren(expr, parent);
                if paren {
                    out.push('(');
                }
                self.rule(arg, false, out, Some(expr));
                out.push(' ');
                out.push_str(test);
                if paren {
                    out.push(')');
                }
            }
            Expr::AnyAll {
                op,
                any,
                left,
                right,
            } => {
                let paren = self.need_paren(expr, parent);
                if paren {
                    out.push('(');
                }
                self.rule(left, true, out, Some(expr));
                out.push(' ');
                out.push_str(op);
                out.push_str(if *any { " ANY (" } else { " ALL (" });
                self.rule(right, true, out, Some(expr));
                out.push(')');
                if paren {
                    out.push(')');
                }
            }
            Expr::Array { elements, ty } => {
                out.push_str("ARRAY[");
                for (i, e) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.rule(e, true, out, None);
                }
                out.push(']');
                if elements.is_empty() {
                    out.push_str("::");
                    out.push_str(&ty.display());
                }
            }
            Expr::Coalesce { args, .. } => self.call("COALESCE", args, out),
            Expr::NullIf { args, .. } => self.call("NULLIF", args, out),
            Expr::MinMax { greatest, args, .. } => {
                self.call(if *greatest { "GREATEST" } else { "LEAST" }, args, out);
            }
            Expr::Case {
                arg,
                whens,
                default,
                ..
            } => {
                out.push_str("CASE");
                if let Some(arg) = arg {
                    out.push(' ');
                    self.rule(arg, true, out, None);
                }
                for (cond, result) in whens {
                    out.push_str(" WHEN ");
                    self.rule(cond, false, out, None);
                    out.push_str(" THEN ");
                    self.rule(result, true, out, None);
                }
                if let Some(d) = default {
                    out.push_str(" ELSE ");
                    self.rule(d, true, out, None);
                }
                out.push_str(" END");
            }
            Expr::SqlValue { text, .. } => out.push_str(text),
            Expr::Collate { arg, collation } => {
                let paren = !self.pretty;
                if paren {
                    out.push('(');
                }
                self.rule(arg, show_implicit, out, Some(expr));
                out.push_str(" COLLATE ");
                out.push_str(collation);
                if paren {
                    out.push(')');
                }
            }
            Expr::IsDistinct { not, left, right } => {
                let paren = self.need_paren(expr, parent);
                if paren {
                    out.push('(');
                }
                self.rule(left, true, out, Some(expr));
                out.push_str(if *not {
                    " IS NOT DISTINCT FROM "
                } else {
                    " IS DISTINCT FROM "
                });
                self.rule(right, true, out, Some(expr));
                if paren {
                    out.push(')');
                }
            }
            Expr::Raw { text, .. } => out.push_str(text),
        }
    }

    fn call(&self, name: &str, args: &[Expr], out: &mut String) {
        out.push_str(name);
        out.push('(');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            self.rule(a, true, out, None);
        }
        out.push(')');
    }

    /// Plain output parenthesises every operator, boolean and test; pretty
    /// output only where the parent would otherwise read it differently.
    fn need_paren(&self, expr: &Expr, parent: Option<&Expr>) -> bool {
        if !self.pretty {
            return true;
        }
        let Some(parent) = parent else {
            return false;
        };
        match (expr, parent) {
            (Expr::Op { op, .. }, Expr::Op { op: parent_op, .. }) => {
                precedence(op) < precedence(parent_op)
                    || (precedence(op) == precedence(parent_op) && is_comparison(op))
            }
            (Expr::Bool { op: inner, .. }, Expr::Bool { op: outer, .. }) => inner != outer,
            (Expr::Op { .. } | Expr::NullTest { .. } | Expr::AnyAll { .. }, Expr::Bool { .. }) => {
                false
            }
            (_, Expr::Cast { .. }) => !simple(expr),
            _ => true,
        }
    }
}

/// Binding strength of an operator, for pretty output.
fn precedence(op: &str) -> u8 {
    match op {
        "*" | "/" | "%" => 6,
        "+" | "-" => 5,
        "^" => 7,
        "=" | "<>" | "<" | ">" | "<=" | ">=" => 2,
        "~~" | "!~~" | "~~*" | "!~~*" => 3,
        _ => 4,
    }
}

/// Nodes that print as one token or call: no parentheses around them.
fn simple(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Const { .. }
            | Expr::Unknown { .. }
            | Expr::Column { .. }
            | Expr::DomainValue { .. }
            | Expr::Func { .. }
            | Expr::SqlValue { .. }
            | Expr::Coalesce { .. }
            | Expr::NullIf { .. }
            | Expr::MinMax { .. }
            | Expr::Array { .. }
            | Expr::Cast { .. }
    )
}

/// `get_const_expr`: integers bare (quoted and labelled when negative),
/// numbers with a point bare, booleans bare, everything else quoted and
/// labelled with its type.
fn const_expr(value: Option<&str>, ty: &Ty, out: &mut String) {
    let Some(value) = value else {
        out.push_str("NULL");
        if !ty.is_unknown() {
            out.push_str("::");
            out.push_str(&ty.display());
        }
        return;
    };
    let label = match &ty.base {
        Base::Builtin("int4") if !ty.array => {
            if value.starts_with('-') {
                out.push_str(&quote_literal(value));
                true
            } else {
                out.push_str(value);
                false
            }
        }
        Base::Builtin("numeric") if !ty.array => {
            let float_like = value.bytes().next().is_some_and(|b| b.is_ascii_digit())
                && value.contains(['.', 'e', 'E']);
            if float_like {
                out.push_str(value);
                !ty.modifier.is_empty()
            } else {
                out.push_str(&quote_literal(value));
                true
            }
        }
        Base::Builtin("bool") if !ty.array => {
            out.push_str(value);
            false
        }
        Base::Builtin("unknown") => {
            out.push_str(&quote_literal(value));
            false
        }
        _ => {
            out.push_str(&quote_literal(value));
            true
        }
    };
    if label {
        out.push_str("::");
        out.push_str(&ty.display());
    }
}

/// An identifier always in double quotes (collations are printed so).
fn quote_always(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The SQL text a node came from: from its first location to the end of
/// the expression, found by balancing parentheses up to a `,` or `)` at
/// depth zero (or a keyword ending the clause).
pub fn source_of(sql: &str, node: &Value) -> String {
    let mut start = usize::MAX;
    tree::visit(node, &mut |_, fields| {
        if let Some(l) = tree::location(fields) {
            start = start.min(l);
        }
    });
    let Some(rest) = sql.get(start..) else {
        return String::new();
    };
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut end = rest.len();
    for (i, c) in rest.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => {
                    if depth == 0 {
                        end = i;
                        break;
                    }
                    depth -= 1;
                }
                ',' | ';' if depth == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            },
        }
    }
    rest.get(..end).unwrap_or(rest).trim().to_owned()
}

fn source_slice(sql: &str, location: Option<usize>, len: usize) -> String {
    location
        .and_then(|l| sql.get(l..l + len))
        .unwrap_or("")
        .to_owned()
}

/// Every run of whitespace outside quotes as one space.
pub fn squash(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut quote = None;
    let mut space = false;
    for c in sql.chars() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None if c.is_whitespace() => space = true,
            None => {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                space = false;
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::normalize_numeric;

    #[test]
    fn numbers_read_as_numeric_prints_them() {
        assert_eq!(normalize_numeric("1.50"), "1.50");
        assert_eq!(normalize_numeric(".5"), "0.5");
        assert_eq!(normalize_numeric("1e3"), "1000");
        assert_eq!(normalize_numeric("2.5e-2"), "0.025");
        assert_eq!(normalize_numeric("-1.5"), "-1.5");
    }
}
