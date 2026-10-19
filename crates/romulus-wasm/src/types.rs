//! Types as Postgres names them: what a `TypeName` of the parse tree
//! resolves to, and how `format_type` prints it (the model's `type`).

use remus_core::{emit::ident::quote_ident, model::TypeKind};

/// How a built-in type's modifier prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    None,
    /// `(n)`: `character varying(20)`, `bit(4)`.
    Length,
    /// `numeric(p,s)`.
    Numeric,
    /// `timestamp(3) with time zone`: before the suffix.
    Precision,
    /// `interval day to second(3)`.
    Interval,
}

/// A built-in type: its catalog name, how `format_type` prints it, and its
/// category, which decides casts and operators here.
#[derive(Debug)]
pub struct Builtin {
    pub name: &'static str,
    display: &'static str,
    /// Printed after the modifier: ` with time zone`.
    suffix: &'static str,
    modifier: Modifier,
    pub category: Category,
    pub kind: TypeKind,
}

/// `pg_type.typcategory`, the parts this engine reasons about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Boolean,
    Numeric,
    String,
    DateTime,
    Timespan,
    Network,
    BitString,
    Geometric,
    Range,
    User,
    Other,
}

macro_rules! builtins {
    ($($name:literal => $display:literal $suffix:literal, $modifier:ident, $category:ident, $kind:ident;)*) => {
        const BUILTINS: &[Builtin] = &[$(Builtin {
            name: $name,
            display: $display,
            suffix: $suffix,
            modifier: Modifier::$modifier,
            category: Category::$category,
            kind: TypeKind::$kind,
        }),*];
    };
}

builtins! {
    "bool" => "boolean" "", None, Boolean, Base;
    "int2" => "smallint" "", None, Numeric, Base;
    "int4" => "integer" "", None, Numeric, Base;
    "int8" => "bigint" "", None, Numeric, Base;
    "numeric" => "numeric" "", Numeric, Numeric, Base;
    "float4" => "real" "", None, Numeric, Base;
    "float8" => "double precision" "", None, Numeric, Base;
    "money" => "money" "", None, Numeric, Base;
    "oid" => "oid" "", None, Numeric, Base;
    "text" => "text" "", None, String, Base;
    "varchar" => "character varying" "", Length, String, Base;
    "bpchar" => "character" "", Length, String, Base;
    "char" => "\"char\"" "", None, String, Base;
    "name" => "name" "", None, String, Base;
    "bytea" => "bytea" "", None, Other, Base;
    "date" => "date" "", None, DateTime, Base;
    "time" => "time" " without time zone", Precision, DateTime, Base;
    "timetz" => "time" " with time zone", Precision, DateTime, Base;
    "timestamp" => "timestamp" " without time zone", Precision, DateTime, Base;
    "timestamptz" => "timestamp" " with time zone", Precision, DateTime, Base;
    "interval" => "interval" "", Interval, Timespan, Base;
    "uuid" => "uuid" "", None, Other, Base;
    "json" => "json" "", None, Other, Base;
    "jsonb" => "jsonb" "", None, Other, Base;
    "jsonpath" => "jsonpath" "", None, Other, Base;
    "xml" => "xml" "", None, Other, Base;
    "inet" => "inet" "", None, Network, Base;
    "cidr" => "cidr" "", None, Network, Base;
    "macaddr" => "macaddr" "", None, Other, Base;
    "macaddr8" => "macaddr8" "", None, Other, Base;
    "bit" => "bit" "", Length, BitString, Base;
    "varbit" => "bit varying" "", Length, BitString, Base;
    "tsvector" => "tsvector" "", None, Other, Base;
    "tsquery" => "tsquery" "", None, Other, Base;
    "point" => "point" "", None, Geometric, Base;
    "line" => "line" "", None, Geometric, Base;
    "lseg" => "lseg" "", None, Geometric, Base;
    "box" => "box" "", None, Geometric, Base;
    "path" => "path" "", None, Geometric, Base;
    "polygon" => "polygon" "", None, Geometric, Base;
    "circle" => "circle" "", None, Geometric, Base;
    "int4range" => "int4range" "", None, Range, Range;
    "int8range" => "int8range" "", None, Range, Range;
    "numrange" => "numrange" "", None, Range, Range;
    "tsrange" => "tsrange" "", None, Range, Range;
    "tstzrange" => "tstzrange" "", None, Range, Range;
    "daterange" => "daterange" "", None, Range, Range;
    "int4multirange" => "int4multirange" "", None, Range, Multirange;
    "int8multirange" => "int8multirange" "", None, Range, Multirange;
    "nummultirange" => "nummultirange" "", None, Range, Multirange;
    "tsmultirange" => "tsmultirange" "", None, Range, Multirange;
    "tstzmultirange" => "tstzmultirange" "", None, Range, Multirange;
    "datemultirange" => "datemultirange" "", None, Range, Multirange;
    "regclass" => "regclass" "", None, Numeric, Base;
    "regconfig" => "regconfig" "", None, Numeric, Base;
    "regtype" => "regtype" "", None, Numeric, Base;
    "regproc" => "regproc" "", None, Numeric, Base;
    "pg_lsn" => "pg_lsn" "", None, Other, Base;
    "txid_snapshot" => "txid_snapshot" "", None, Other, Base;
    "pg_snapshot" => "pg_snapshot" "", None, Other, Base;
    "record" => "record" "", None, Other, Pseudo;
    "unknown" => "unknown" "", None, String, Pseudo;
}

pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Spellings Postgres's type lookup also accepts for built-ins (`bool`,
/// `float8`…: catalog names are looked up as they are; these are the
/// grammar's keywords that reach the tree unqualified).
fn alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "integer" | "int" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "boolean" => "bool",
        "real" => "float4",
        "decimal" => "numeric",
        "timestamptz" => "timestamptz",
        "timetz" => "timetz",
        _ => return None,
    })
}

/// Serial pseudo-types: the column's type, and the label of its sequence.
pub fn serial(name: &str) -> Option<&'static str> {
    Some(match name {
        "serial" | "serial4" => "int4",
        "bigserial" | "serial8" => "int8",
        "smallserial" | "serial2" => "int2",
        _ => return None,
    })
}

/// A resolved type, with its modifier and whether it is an array.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ty {
    pub base: Base,
    /// The modifier's printed form, e.g. `(20)`, `(12,2)`, ` day to second`;
    /// empty when there is none.
    pub modifier: String,
    pub array: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Base {
    Builtin(&'static str),
    /// A type of the text, or of an extension: schema, name, kind.
    User {
        schema: String,
        name: String,
        kind: UserKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserKind {
    Enum,
    Domain,
    Composite,
    Range,
    /// An extension's base type (`citext`, `vector`, `geometry`).
    Extension,
}

impl Ty {
    pub fn builtin(name: &'static str) -> Self {
        Self {
            base: Base::Builtin(name),
            modifier: String::new(),
            array: false,
        }
    }

    pub fn unknown() -> Self {
        Self::builtin("unknown")
    }

    pub fn is(&self, name: &str) -> bool {
        !self.array && matches!(self.base, Base::Builtin(n) if n == name)
    }

    pub fn is_unknown(&self) -> bool {
        self.is("unknown")
    }

    #[must_use]
    pub fn element(&self) -> Self {
        Self {
            array: false,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn array_of(&self) -> Self {
        Self {
            array: true,
            ..self.clone()
        }
    }

    /// Without its modifier: what a cast to it leaves on a constant.
    #[must_use]
    pub fn plain(&self) -> Self {
        Self {
            modifier: String::new(),
            ..self.clone()
        }
    }

    pub fn category(&self) -> Category {
        if self.array {
            return Category::Other;
        }
        match &self.base {
            Base::Builtin(name) => builtin(name).map_or(Category::Other, |b| b.category),
            Base::User {
                kind: UserKind::Enum | UserKind::Composite,
                ..
            } => Category::User,
            Base::User { .. } => Category::Other,
        }
    }

    /// `format_type`'s spelling, as the model's `type` holds it: built-ins
    /// by their SQL names, others schema-qualified (introspection runs with
    /// an empty `search_path`).
    pub fn display(&self) -> String {
        let mut out = match &self.base {
            // A `bpchar` without a length is not `character`, which means
            // `character(1)`: format_type says `bpchar`.
            Base::Builtin(name) => match builtin(name) {
                Some(b) if self.modifier.is_empty() && b.name == "bpchar" => "bpchar".to_owned(),
                Some(b) => format!("{}{}{}", b.display, self.modifier, b.suffix),
                None => format!("{name}{}", self.modifier),
            },
            Base::User { schema, name, .. } => {
                format!(
                    "{}.{}{}",
                    quote_ident(schema),
                    quote_ident(name),
                    self.modifier
                )
            }
        };
        if self.array {
            out.push_str("[]");
        }
        out
    }

    /// The model's `type_schema` and `type_name` (of the element type).
    pub fn catalog_name(&self) -> (String, String) {
        match &self.base {
            Base::Builtin(name) => ("pg_catalog".to_owned(), (*name).to_owned()),
            Base::User { schema, name, .. } => (schema.clone(), name.clone()),
        }
    }

    pub fn kind(&self) -> TypeKind {
        match &self.base {
            Base::Builtin(name) => builtin(name).map_or(TypeKind::Base, |b| b.kind.clone()),
            Base::User { kind, .. } => match kind {
                UserKind::Enum => TypeKind::Enum,
                UserKind::Domain => TypeKind::Domain,
                UserKind::Composite => TypeKind::Composite,
                UserKind::Range => TypeKind::Range,
                UserKind::Extension => TypeKind::Base,
            },
        }
    }
}

/// The modifier of a built-in from the integer typmods written, as
/// `format_type` prints it, or why Postgres refuses it.
pub fn builtin_modifier(name: &str, typmods: &[i64]) -> Result<String, String> {
    let Some(b) = builtin(name) else {
        return Ok(String::new());
    };
    if typmods.is_empty() {
        return Ok(String::new());
    }
    match b.modifier {
        Modifier::None => Err(format!(
            "type modifier is not allowed for type \"{}\"",
            b.display.trim_matches('"')
        )),
        Modifier::Length => {
            let [n] = typmods else {
                return Err(format!("invalid type modifier for {}", b.display));
            };
            if *n < 1 {
                return Err(format!(
                    "length for type {} must be at least 1",
                    if name == "bpchar" { "char" } else { name }
                ));
            }
            Ok(format!("({n})"))
        }
        Modifier::Numeric => match typmods {
            [p] if (1..=1000).contains(p) => Ok(format!("({p},0)")),
            [p, s] if (1..=1000).contains(p) && (-1000..=1000).contains(s) => {
                Ok(format!("({p},{s})"))
            }
            [p, ..] if !(1..=1000).contains(p) => {
                Err(format!("NUMERIC precision {p} must be between 1 and 1000"))
            }
            _ => Err("invalid NUMERIC type modifier".to_owned()),
        },
        Modifier::Precision => match typmods {
            [p] if (0..=6).contains(p) => Ok(format!("({p})")),
            [p] if *p > 6 => Ok("(6)".to_owned()),
            _ => Err(format!("invalid type modifier for {}", b.display)),
        },
        Modifier::Interval => Ok(interval_modifier(typmods)),
    }
}

const INTERVAL_FULL_RANGE: i64 = 0x7FFF;

/// `intervaltypmodout`: the fields (`day to second`) and the precision.
fn interval_modifier(typmods: &[i64]) -> String {
    const YEAR: i64 = 1 << 2;
    const MONTH: i64 = 1 << 1;
    const DAY: i64 = 1 << 3;
    const HOUR: i64 = 1 << 10;
    const MINUTE: i64 = 1 << 11;
    const SECOND: i64 = 1 << 12;
    let range = typmods.first().copied().unwrap_or(INTERVAL_FULL_RANGE);
    let fields = match range {
        r if r == YEAR => " year",
        r if r == MONTH => " month",
        r if r == DAY => " day",
        r if r == HOUR => " hour",
        r if r == MINUTE => " minute",
        r if r == SECOND => " second",
        r if r == YEAR | MONTH => " year to month",
        r if r == DAY | HOUR => " day to hour",
        r if r == DAY | HOUR | MINUTE => " day to minute",
        r if r == DAY | HOUR | MINUTE | SECOND => " day to second",
        r if r == HOUR | MINUTE => " hour to minute",
        r if r == HOUR | MINUTE | SECOND => " hour to second",
        r if r == MINUTE | SECOND => " minute to second",
        _ => "",
    };
    match typmods.get(1) {
        Some(p) => format!("{fields}({})", p.clamp(&0, &6)),
        None => fields.to_owned(),
    }
}

/// Resolves an unqualified type name among the built-ins, as the lookup in
/// `pg_catalog` (always first on the path) would.
pub fn builtin_named(name: &str) -> Option<&'static str> {
    builtin(name).map(|b| b.name).or_else(|| alias(name))
}

/// What an extension provides: its version (the sandbox's) and its types.
pub struct ExtensionInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub types: &'static [&'static str],
    /// Functions it adds, for defaults and checks.
    pub functions: &'static [(&'static str, &'static str)],
    pub requires: &'static [&'static str],
}

/// The extensions the app's sandbox offers (romulus-sandbox's allowlist),
/// at the versions its image installs.
pub const EXTENSIONS: &[ExtensionInfo] = &[
    ExtensionInfo {
        name: "btree_gin",
        version: "1.3",
        types: &[],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "btree_gist",
        version: "1.7",
        types: &[],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "citext",
        version: "1.6",
        types: &["citext"],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "cube",
        version: "1.5",
        types: &["cube"],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "earthdistance",
        version: "1.2",
        types: &["earth"],
        functions: &[],
        requires: &["cube"],
    },
    ExtensionInfo {
        name: "fuzzystrmatch",
        version: "1.2",
        types: &[],
        functions: &[("soundex", "text"), ("levenshtein", "int4")],
        requires: &[],
    },
    ExtensionInfo {
        name: "hstore",
        version: "1.8",
        types: &["hstore"],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "intarray",
        version: "1.5",
        types: &[],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "isn",
        version: "1.2",
        types: &[
            "isbn", "isbn13", "ismn", "ismn13", "issn", "issn13", "ean13", "upc",
        ],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "ltree",
        version: "1.3",
        types: &["ltree", "lquery", "ltxtquery"],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "pg_trgm",
        version: "1.6",
        types: &[],
        functions: &[("similarity", "float4")],
        requires: &[],
    },
    ExtensionInfo {
        name: "pgcrypto",
        version: "1.3",
        types: &[],
        functions: &[
            ("crypt", "text"),
            ("gen_salt", "text"),
            ("digest", "bytea"),
            ("hmac", "bytea"),
        ],
        requires: &[],
    },
    ExtensionInfo {
        name: "plpgsql",
        version: "1.0",
        types: &[],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "postgis",
        version: "3.6.4",
        types: &["geometry", "geography", "box2d", "box3d"],
        functions: &[("st_makepoint", "geometry"), ("st_setsrid", "geometry")],
        requires: &[],
    },
    ExtensionInfo {
        name: "postgis_topology",
        version: "3.6.4",
        types: &[],
        functions: &[],
        requires: &["postgis"],
    },
    ExtensionInfo {
        name: "seg",
        version: "1.4",
        types: &["seg"],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "tsm_system_rows",
        version: "1.0",
        types: &[],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "tsm_system_time",
        version: "1.0",
        types: &[],
        functions: &[],
        requires: &[],
    },
    ExtensionInfo {
        name: "unaccent",
        version: "1.1",
        types: &[],
        functions: &[("unaccent", "text")],
        requires: &[],
    },
    ExtensionInfo {
        name: "uuid-ossp",
        version: "1.1",
        types: &[],
        functions: &[
            ("uuid_generate_v1", "uuid"),
            ("uuid_generate_v4", "uuid"),
            ("uuid_nil", "uuid"),
        ],
        requires: &[],
    },
    ExtensionInfo {
        name: "vector",
        version: "0.8.7",
        types: &["vector", "halfvec", "sparsevec"],
        functions: &[],
        requires: &[],
    },
];

pub fn extension(name: &str) -> Option<&'static ExtensionInfo> {
    EXTENSIONS.iter().find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::{Base, Ty, UserKind, builtin_modifier};

    fn ty(name: &'static str, typmods: &[i64]) -> String {
        Ty {
            modifier: builtin_modifier(name, typmods).unwrap(),
            ..Ty::builtin(name)
        }
        .display()
    }

    #[test]
    fn prints_like_format_type() {
        assert_eq!(ty("varchar", &[20]), "character varying(20)");
        assert_eq!(ty("varchar", &[]), "character varying");
        assert_eq!(ty("bpchar", &[3]), "character(3)");
        assert_eq!(ty("numeric", &[12, 2]), "numeric(12,2)");
        assert_eq!(ty("numeric", &[12]), "numeric(12,0)");
        assert_eq!(ty("timestamptz", &[0]), "timestamp(0) with time zone");
        assert_eq!(ty("timestamp", &[]), "timestamp without time zone");
        assert_eq!(ty("interval", &[7176]), "interval day to second");
        assert_eq!(ty("interval", &[0x7FFF, 3]), "interval(3)");
        assert_eq!(ty("varbit", &[]), "bit varying");
        assert_eq!(ty("float8", &[]), "double precision");
        let mood = Ty {
            base: Base::User {
                schema: "public".into(),
                name: "Mood".into(),
                kind: UserKind::Enum,
            },
            modifier: String::new(),
            array: true,
        };
        assert_eq!(mood.display(), "public.\"Mood\"[]");
    }

    #[test]
    fn refuses_what_postgres_refuses() {
        assert!(builtin_modifier("int4", &[3]).is_err());
        assert!(builtin_modifier("varchar", &[0]).is_err());
        assert!(builtin_modifier("numeric", &[2000]).is_err());
    }
}
