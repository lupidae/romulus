//! A column's type, as the generator needs it: what values look like, their
//! bounds, and the domain checks they carry.

use remus_core::{
    Schema,
    model::{Column, TypeKind},
};

use crate::expr::{self, Expr};

/// A type, after domains are resolved to their base.
#[derive(Debug, Clone, PartialEq)]
pub enum Base {
    Int2,
    Int4,
    Int8,
    Float4,
    Float8,
    /// `numeric(p, s)`; unconstrained when `precision` is `None`.
    Numeric {
        precision: Option<u32>,
        scale: u32,
    },
    Money,
    Text,
    Varchar(Option<u32>),
    /// `character(n)`: blank-padded, `n` characters.
    Char(u32),
    /// `"char"`: one byte.
    InternalChar,
    Name,
    Citext,
    Bool,
    Uuid,
    Date,
    Time {
        tz: bool,
    },
    Timestamp {
        tz: bool,
        precision: Option<u32>,
    },
    Interval,
    Bytea,
    Inet,
    Cidr,
    Macaddr,
    Macaddr8,
    Json,
    Jsonb,
    Xml,
    Bit(u32),
    VarBit(Option<u32>),
    TsVector,
    TsQuery,
    Point,
    Line,
    Lseg,
    Box,
    Path,
    Polygon,
    Circle,
    Oid,
    PgLsn,
    Hstore,
    Ltree,
    /// PostGIS: `geometry(Point, 4326)`, the shape and SRID from the typmod.
    Geometry {
        geography: bool,
        shape: Option<String>,
        srid: Option<i32>,
    },
    /// pgvector: `vector(n)`, `halfvec(n)`, `sparsevec(n)`.
    Vector {
        kind: VectorKind,
        dims: Option<u32>,
    },
    Cube,
    Range(RangeKind),
    Multirange(RangeKind),
    Enum {
        name: String,
        labels: Vec<String>,
    },
    Composite {
        name: String,
        attributes: Vec<(String, Type)>,
    },
    /// Anything else: named for the report.
    Unknown(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorKind {
    Vector,
    HalfVec,
    SparseVec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    Int4,
    Int8,
    Num,
    Ts,
    TsTz,
    Date,
}

impl RangeKind {
    pub fn element(self) -> Base {
        match self {
            RangeKind::Int4 => Base::Int4,
            RangeKind::Int8 => Base::Int8,
            RangeKind::Num => Base::Numeric {
                precision: None,
                scale: 2,
            },
            RangeKind::Ts => Base::Timestamp {
                tz: false,
                precision: None,
            },
            RangeKind::TsTz => Base::Timestamp {
                tz: true,
                precision: None,
            },
            RangeKind::Date => Base::Date,
        }
    }
}

/// A domain's constraints, flattened over domains of domains.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DomainInfo {
    pub names: Vec<String>,
    pub not_null: bool,
    /// Each CHECK, parsed (`VALUE` is the value); `None` when unreadable.
    pub checks: Vec<(String, Option<Expr>)>,
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub base: Base,
    /// A one-dimensional array of `base`.
    pub array: bool,
    pub domain: Option<DomainInfo>,
}

impl Type {
    pub fn scalar(base: Base) -> Self {
        Self {
            base,
            array: false,
            domain: None,
        }
    }

    /// The number of distinct values, when small enough to count.
    pub fn domain_size(&self) -> Option<u64> {
        if self.array {
            return None;
        }
        match &self.base {
            Base::Bool => Some(2),
            Base::Enum { labels, .. } => Some(labels.len() as u64),
            Base::Int2 => Some(65_536),
            Base::Char(1) | Base::InternalChar => Some(36),
            Base::Bit(n) if *n < 20 => Some(1 << n),
            _ => None,
        }
    }

    pub fn is_text(&self) -> bool {
        !self.array
            && matches!(
                self.base,
                Base::Text | Base::Varchar(_) | Base::Char(_) | Base::Citext | Base::Name
            )
    }

    pub fn is_integer(&self) -> bool {
        !self.array && matches!(self.base, Base::Int2 | Base::Int4 | Base::Int8)
    }

    pub fn is_numeric(&self) -> bool {
        !self.array
            && matches!(
                self.base,
                Base::Int2
                    | Base::Int4
                    | Base::Int8
                    | Base::Float4
                    | Base::Float8
                    | Base::Numeric { .. }
                    | Base::Money
            )
    }

    pub fn is_temporal(&self) -> bool {
        !self.array && matches!(self.base, Base::Date | Base::Timestamp { .. })
    }

    /// The longest text it holds, in characters.
    pub fn max_chars(&self) -> Option<usize> {
        match self.base {
            Base::Varchar(Some(n)) | Base::Char(n) => Some(n as usize),
            Base::Name => Some(63),
            Base::InternalChar => Some(1),
            _ => None,
        }
    }

    /// Can a value of this type be told apart from row to row (keys)?
    pub fn can_be_unique(&self) -> bool {
        if self.array {
            return true;
        }
        // Short types hold few values: the row count is capped to them.
        match &self.base {
            Base::Bool | Base::Enum { .. } | Base::Unknown(_) => false,
            Base::Composite { attributes, .. } => attributes.iter().any(|(_, t)| t.can_be_unique()),
            _ => true,
        }
    }
}

/// Resolves type names against a model.
pub struct Types<'a> {
    pub model: &'a Schema,
}

impl Types<'_> {
    pub fn column(&self, column: &Column) -> Type {
        // The kind and catalog name are exact; the printed type carries
        // the modifier and the array brackets.
        let printed = column.r#type.trim();
        let (scalar, dims) = strip_array(printed);
        let mut ty = match column.type_kind {
            TypeKind::Domain => self
                .domain(&column.type_schema, &column.type_name, 0)
                .unwrap_or_else(|| Type::scalar(Base::Unknown(printed.to_owned()))),
            TypeKind::Enum => self
                .enum_type(&column.type_schema, &column.type_name)
                .map_or_else(
                    || Type::scalar(Base::Unknown(printed.to_owned())),
                    Type::scalar,
                ),
            TypeKind::Composite => self
                .composite(&column.type_schema, &column.type_name, 0)
                .map_or_else(
                    || Type::scalar(Base::Unknown(printed.to_owned())),
                    Type::scalar,
                ),
            TypeKind::Range => Type::scalar(range(&column.type_name, false)),
            TypeKind::Multirange => Type::scalar(range(&column.type_name, true)),
            TypeKind::Pseudo => Type::scalar(Base::Unknown(printed.to_owned())),
            TypeKind::Base => Type::scalar(base(&column.type_name, modifier(scalar))),
        };
        ty.array = column.is_array || dims > 0;
        ty
    }

    /// A type as `format_type` prints it (composite attributes, domain bases).
    pub fn parse(&self, printed: &str, depth: usize) -> Type {
        let printed = printed.trim();
        let (scalar, dims) = strip_array(printed);
        let name = scalar.find('(').map_or(scalar, |i| scalar[..i].trim_end());
        let modifier = modifier(scalar);
        // `timestamp(3) with time zone`: the modifier sits in the middle.
        let words = scalar
            .split(['(', ')'])
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, s)| s.trim())
            .collect::<Vec<_>>()
            .join(" ");
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        let catalog = match words.as_str() {
            "smallint" => Some("int2"),
            "integer" | "int" => Some("int4"),
            "bigint" => Some("int8"),
            "real" => Some("float4"),
            "double precision" => Some("float8"),
            "numeric" | "decimal" => Some("numeric"),
            "money" => Some("money"),
            "text" => Some("text"),
            "character varying" => Some("varchar"),
            "character" => Some("bpchar"),
            "\"char\"" => Some("char"),
            "name" => Some("name"),
            "boolean" => Some("bool"),
            "uuid" => Some("uuid"),
            "date" => Some("date"),
            "time without time zone" | "time" => Some("time"),
            "time with time zone" => Some("timetz"),
            "timestamp without time zone" | "timestamp" => Some("timestamp"),
            "timestamp with time zone" => Some("timestamptz"),
            "bytea" => Some("bytea"),
            "inet" => Some("inet"),
            "cidr" => Some("cidr"),
            "macaddr" => Some("macaddr"),
            "macaddr8" => Some("macaddr8"),
            "json" => Some("json"),
            "jsonb" => Some("jsonb"),
            "xml" => Some("xml"),
            "bit" => Some("bit"),
            "bit varying" => Some("varbit"),
            "tsvector" => Some("tsvector"),
            "tsquery" => Some("tsquery"),
            "point" | "line" | "lseg" | "box" | "path" | "polygon" | "circle" | "oid"
            | "pg_lsn" => Some(name),
            w if w.starts_with("interval") => Some("interval"),
            _ => None,
        };
        let mut ty = if let Some(catalog) = catalog {
            Type::scalar(base(catalog, modifier))
        } else if let Some(kind) = range_kind(
            name.trim_end_matches("multirange")
                .trim_end_matches("range"),
        ) {
            if name.ends_with("multirange") {
                Type::scalar(Base::Multirange(kind))
            } else {
                Type::scalar(Base::Range(kind))
            }
        } else {
            let (schema, bare) = split_qualified(name);
            let schema = schema.unwrap_or_else(|| "public".to_owned());
            if let Some(t) = self.domain(&schema, &bare, depth + 1) {
                t
            } else if let Some(b) = self.enum_type(&schema, &bare) {
                Type::scalar(b)
            } else if let Some(b) = self.composite(&schema, &bare, depth + 1) {
                Type::scalar(b)
            } else {
                Type::scalar(base(&bare, modifier))
            }
        };
        ty.array |= dims > 0;
        ty
    }

    fn enum_type(&self, schema: &str, name: &str) -> Option<Base> {
        self.model
            .enums
            .iter()
            .find(|e| e.schema == schema && e.name == name)
            .map(|e| Base::Enum {
                name: qualified(&e.schema, &e.name),
                labels: e.labels.clone(),
            })
    }

    fn composite(&self, schema: &str, name: &str, depth: usize) -> Option<Base> {
        if depth > 8 {
            return None;
        }
        let c = self
            .model
            .composites
            .iter()
            .find(|c| c.schema == schema && c.name == name)?;
        Some(Base::Composite {
            name: qualified(&c.schema, &c.name),
            attributes: c
                .attributes
                .iter()
                .map(|a| (a.name.clone(), self.parse(&a.r#type, depth + 1)))
                .collect(),
        })
    }

    fn domain(&self, schema: &str, name: &str, depth: usize) -> Option<Type> {
        if depth > 8 {
            return None;
        }
        let d = self
            .model
            .domains
            .iter()
            .find(|d| d.schema == schema && d.name == name)?;
        let mut ty = self.parse(&d.base_type, depth + 1);
        let info = ty.domain.get_or_insert_with(DomainInfo::default);
        info.names.push(qualified(&d.schema, &d.name));
        info.not_null |= d.not_null;
        if info.default.is_none() {
            info.default.clone_from(&d.default);
        }
        for check in &d.constraints {
            info.checks
                .push((check.clone(), expr::parse_check(check, true)));
        }
        Some(ty)
    }
}

pub fn qualified(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

/// `text[]` → (`text`, 1).
fn strip_array(printed: &str) -> (&str, usize) {
    let mut s = printed;
    let mut dims = 0;
    while let Some(rest) = s.strip_suffix("[]") {
        s = rest.trim_end();
        dims += 1;
    }
    (s, dims)
}

/// The modifier in a printed type: `numeric(12,2)` → `12,2`.
fn modifier(printed: &str) -> Option<String> {
    let start = printed.find('(')?;
    let end = printed[start..].find(')')? + start;
    Some(printed[start + 1..end].to_owned())
}

fn split_qualified(name: &str) -> (Option<String>, String) {
    // Quoted parts: `billing."Currency"`.
    let unquote = |s: &str| {
        s.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .map_or_else(|| s.to_owned(), |s| s.replace("\"\"", "\""))
    };
    let mut in_quotes = false;
    for (i, c) in name.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => return (Some(unquote(&name[..i])), unquote(&name[i + 1..])),
            _ => {}
        }
    }
    (None, unquote(name))
}

fn range_kind(prefix: &str) -> Option<RangeKind> {
    Some(match prefix {
        "int4" => RangeKind::Int4,
        "int8" => RangeKind::Int8,
        "num" => RangeKind::Num,
        "ts" => RangeKind::Ts,
        "tstz" => RangeKind::TsTz,
        "date" => RangeKind::Date,
        _ => return None,
    })
}

fn range(type_name: &str, multi: bool) -> Base {
    let prefix = type_name
        .strip_suffix(if multi { "multirange" } else { "range" })
        .unwrap_or("");
    match range_kind(prefix) {
        Some(kind) if multi => Base::Multirange(kind),
        Some(kind) => Base::Range(kind),
        None => Base::Unknown(type_name.to_owned()),
    }
}

fn numbers(modifier: Option<&str>) -> Vec<u32> {
    modifier
        .map(|m| m.split(',').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// A base type by its catalog name (`int4`, `varchar`…) and modifier.
fn base(type_name: &str, modifier: Option<String>) -> Base {
    let m = modifier.as_deref();
    let n = numbers(m);
    match type_name {
        "int2" => Base::Int2,
        "int4" => Base::Int4,
        "int8" => Base::Int8,
        "float4" => Base::Float4,
        "float8" => Base::Float8,
        "numeric" => Base::Numeric {
            precision: n.first().copied(),
            scale: n.get(1).copied().unwrap_or(0),
        },
        "money" => Base::Money,
        "text" => Base::Text,
        "varchar" => Base::Varchar(n.first().copied()),
        "bpchar" => Base::Char(n.first().copied().unwrap_or(1)),
        "char" => Base::InternalChar,
        "name" => Base::Name,
        "citext" => Base::Citext,
        "bool" => Base::Bool,
        "uuid" => Base::Uuid,
        "date" => Base::Date,
        "time" => Base::Time { tz: false },
        "timetz" => Base::Time { tz: true },
        "timestamp" => Base::Timestamp {
            tz: false,
            precision: n.first().copied(),
        },
        "timestamptz" => Base::Timestamp {
            tz: true,
            precision: n.first().copied(),
        },
        "interval" => Base::Interval,
        "bytea" => Base::Bytea,
        "inet" => Base::Inet,
        "cidr" => Base::Cidr,
        "macaddr" => Base::Macaddr,
        "macaddr8" => Base::Macaddr8,
        "json" => Base::Json,
        "jsonb" => Base::Jsonb,
        "xml" => Base::Xml,
        "bit" => Base::Bit(n.first().copied().unwrap_or(1)),
        "varbit" => Base::VarBit(n.first().copied()),
        "tsvector" => Base::TsVector,
        "tsquery" => Base::TsQuery,
        "point" => Base::Point,
        "line" => Base::Line,
        "lseg" => Base::Lseg,
        "box" => Base::Box,
        "path" => Base::Path,
        "polygon" => Base::Polygon,
        "circle" => Base::Circle,
        "oid" => Base::Oid,
        "pg_lsn" => Base::PgLsn,
        "hstore" => Base::Hstore,
        "ltree" => Base::Ltree,
        "cube" => Base::Cube,
        "geometry" | "geography" => {
            let parts: Vec<&str> = m
                .map(|m| m.split(',').map(str::trim).collect())
                .unwrap_or_default();
            Base::Geometry {
                geography: type_name == "geography",
                shape: parts.first().map(|s| s.to_lowercase()),
                srid: parts.get(1).and_then(|s| s.parse().ok()),
            }
        }
        "vector" | "halfvec" | "sparsevec" => Base::Vector {
            kind: match type_name {
                "vector" => VectorKind::Vector,
                "halfvec" => VectorKind::HalfVec,
                _ => VectorKind::SparseVec,
            },
            dims: n.first().copied(),
        },
        other => Base::Unknown(match modifier {
            Some(m) => format!("{other}({m})"),
            None => other.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Schema {
        Schema::from_json(
            r#"{"remus_version": "0.2", "database": "d", "server_version": "17",
            "enums": [{"schema": "public", "name": "mood", "labels": ["sad", "ok"], "comment": null}],
            "domains": [
                {"schema": "public", "name": "cents", "base_type": "bigint", "not_null": true,
                 "default": "0", "constraints": ["CHECK ((VALUE >= 0))"], "comment": null},
                {"schema": "public", "name": "small_cents", "base_type": "public.cents", "not_null": false,
                 "default": null, "constraints": ["CHECK ((VALUE < 100))"], "comment": null}
            ],
            "composites": [{"schema": "public", "name": "address", "comment": null,
                "attributes": [{"name": "country", "type": "character(2)"}, {"name": "mood", "type": "public.mood"}]}]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn printed_types_resolve() {
        let m = model();
        let t = Types { model: &m };
        assert_eq!(
            t.parse("character varying(255)", 0).base,
            Base::Varchar(Some(255))
        );
        assert_eq!(
            t.parse("numeric(12,2)", 0).base,
            Base::Numeric {
                precision: Some(12),
                scale: 2
            }
        );
        assert_eq!(
            t.parse("timestamp(3) with time zone", 0).base,
            Base::Timestamp {
                tz: true,
                precision: Some(3)
            }
        );
        assert_eq!(t.parse("interval day to second", 0).base, Base::Interval);
        assert!(t.parse("text[]", 0).array);
        assert_eq!(
            t.parse("public.geometry(Point,4326)", 0).base,
            Base::Geometry {
                geography: false,
                shape: Some("point".into()),
                srid: Some(4326)
            }
        );
        assert_eq!(
            t.parse("public.vector(1536)", 0).base,
            Base::Vector {
                kind: VectorKind::Vector,
                dims: Some(1536)
            }
        );
        assert_eq!(
            t.parse("tstzmultirange", 0).base,
            Base::Multirange(RangeKind::TsTz)
        );
        assert_eq!(t.parse("\"char\"", 0).base, Base::InternalChar);
    }

    #[test]
    fn domains_flatten_with_every_check() {
        let m = model();
        let t = Types { model: &m };
        let ty = t.parse("public.small_cents", 0);
        assert_eq!(ty.base, Base::Int8);
        let d = ty.domain.unwrap();
        assert!(d.not_null);
        assert_eq!(d.checks.len(), 2);
        assert_eq!(d.default.as_deref(), Some("0"));
    }

    #[test]
    fn composites_resolve_their_attributes() {
        let m = model();
        let t = Types { model: &m };
        let Base::Composite { attributes, .. } = t.parse("public.address", 0).base else {
            panic!()
        };
        assert_eq!(attributes[0].1.base, Base::Char(2));
        assert!(matches!(attributes[1].1.base, Base::Enum { .. }));
    }
}
