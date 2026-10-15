//! What the sandbox provides without the text creating it: built-in
//! functions, and the extensions it offers with what each one creates.
//!
//! The compiler loads a [`Catalog`] from the sandbox when it starts (see
//! [`crate::Compiler::start`]); the pure parts of the crate take one as an
//! argument so they can be tested with a hand-made catalog.

use std::collections::{BTreeMap, HashSet};

/// Extensions `CREATE EXTENSION` may name. The sandbox image ships them and
/// marks them trusted (deploy/sandbox/trusted-extensions.txt, plus plpgsql,
/// which every database has). Deliberately absent: anything that reaches
/// outside the database (`dblink`, `postgres_fdw`, `file_fdw`), reads the
/// server's files or statistics (`adminpack`, `pg_stat_statements`,
/// `pgstattuple`), or runs
/// SQL given as a string (tablefunc's crosstab).
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    "btree_gin",
    "btree_gist",
    "citext",
    "cube",
    "earthdistance",
    "fuzzystrmatch",
    "hstore",
    "intarray",
    "isn",
    "ltree",
    "pg_trgm",
    "pgcrypto",
    "plpgsql",
    "postgis",
    "postgis_raster",
    "postgis_topology",
    "seg",
    "tsm_system_rows",
    "tsm_system_time",
    "unaccent",
    "uuid-ossp",
    "vector",
];

/// Built-in functions and the contents of each available extension.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    builtin_functions: HashSet<String>,
    extensions: BTreeMap<String, Extension>,
}

/// What `CREATE EXTENSION` of one extension creates, as measured by
/// installing it into an empty database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extension {
    /// Extensions it requires (`CASCADE` installs them too).
    pub requires: Vec<String>,
    /// Whether `WITH SCHEMA` can choose its schema. When it cannot, its
    /// objects go to `fixed_schema`.
    pub relocatable: bool,
    /// The schema its control file imposes, if any (`topology`).
    pub fixed_schema: Option<String>,
    /// Names of the functions it creates.
    pub functions: HashSet<String>,
    /// Names of the types it creates (base types like `geometry` included).
    pub types: HashSet<String>,
    /// Relations and stand-alone types that remus would list, and render
    /// back as SQL: PostGIS's `spatial_ref_sys`, `geometry_columns`,
    /// `geometry_dump`… A text compiled from such a model creates them
    /// twice, so the compiler skips those statements.
    pub objects: Vec<ProvidedObject>,
}

/// An object an extension creates. `schema` is `None` for objects created
/// in the extension's own schema (wherever `CREATE EXTENSION` puts it).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProvidedObject {
    pub schema: Option<String>,
    pub name: String,
    pub kind: ProvidedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProvidedKind {
    /// A table, view, materialized view or sequence (one namespace).
    Relation,
    /// A composite, enum, domain or range type.
    Type,
}

impl Catalog {
    pub fn new(
        builtin_functions: impl IntoIterator<Item = String>,
        extensions: impl IntoIterator<Item = (String, Extension)>,
    ) -> Self {
        Self {
            builtin_functions: builtin_functions.into_iter().collect(),
            extensions: extensions.into_iter().collect(),
        }
    }

    /// A function of `pg_catalog`, by name (any signature).
    pub fn is_builtin_function(&self, name: &str) -> bool {
        self.builtin_functions.contains(name)
    }

    pub fn extension(&self, name: &str) -> Option<&Extension> {
        self.extensions.get(name)
    }

    /// Available extensions, by name.
    pub fn extensions(&self) -> impl Iterator<Item = (&str, &Extension)> {
        self.extensions.iter().map(|(n, e)| (n.as_str(), e))
    }

    /// An extension that defines a function of this name, for hints.
    pub fn extension_with_function(&self, name: &str) -> Option<&str> {
        self.extensions
            .iter()
            .find(|(_, e)| e.functions.contains(name))
            .map(|(n, _)| n.as_str())
    }

    /// An extension that defines a type of this name, for hints.
    pub fn extension_with_type(&self, name: &str) -> Option<&str> {
        self.extensions
            .iter()
            .find(|(_, e)| e.types.contains(name))
            .map(|(n, _)| n.as_str())
    }

    /// `name` and every extension it requires, transitively, dependencies
    /// first.
    pub fn with_requirements(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_requirements(name, &mut out, 0);
        out
    }

    fn collect_requirements(&self, name: &str, out: &mut Vec<String>, depth: usize) {
        if depth > 16 || out.iter().any(|n| n == name) {
            return;
        }
        if let Some(extension) = self.extensions.get(name) {
            for required in &extension.requires {
                self.collect_requirements(required, out, depth + 1);
            }
        }
        out.push(name.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::{Catalog, Extension};

    #[test]
    fn requirements_come_first_and_once() {
        let catalog = Catalog::new(
            [],
            [
                ("postgis".to_owned(), Extension::default()),
                (
                    "postgis_raster".to_owned(),
                    Extension {
                        requires: vec!["postgis".to_owned()],
                        ..Extension::default()
                    },
                ),
                (
                    "both".to_owned(),
                    Extension {
                        requires: vec!["postgis_raster".to_owned(), "postgis".to_owned()],
                        ..Extension::default()
                    },
                ),
            ],
        );
        assert_eq!(
            catalog.with_requirements("both"),
            ["postgis", "postgis_raster", "both"]
        );
        assert_eq!(catalog.with_requirements("unknown"), ["unknown"]);
    }
}
