//! Everything else a schema holds: schemas, types (enums, domains,
//! composites, ranges), views, policies, comments, renames, `SET SCHEMA`,
//! `DROP`, extensions and sequences.

use remus_core::model::{ConstraintKind, PolicyCommand};
use serde_json::Value;

use crate::{
    catalog::{Column, Composite, Domain, EnumType, Extension, Policy, Rel, RelKind},
    ddl::{DomainScope, Fail, Result, Run, TableScope, display, resolve_type},
    diagnostic::{Code, Diagnostic},
    expr::{self, Analyzer, Expr},
    names::choose_constraint_name,
    tree::{self, Name},
    types::{self, Base, Ty, UserKind},
    view,
};

impl Run<'_> {
    pub fn create_schema(&mut self, f: &Value) -> Result<()> {
        let name = tree::str(f, "schemaname").to_owned();
        if self.catalog.schemas.contains(&name) {
            if tree::bool(f, "if_not_exists") {
                return Ok(());
            }
            return Err(Fail::new(
                "42P06",
                format!("schema \"{name}\" already exists"),
            ));
        }
        if name.starts_with("pg_") {
            return Err(
                Fail::new("42939", format!("unacceptable schema name \"{name}\""))
                    .detail("The prefix \"pg_\" is reserved for system schemas."),
            );
        }
        self.catalog.schemas.push(name.clone());
        // `CREATE SCHEMA s CREATE TABLE …`: the elements are created in it.
        let elements = tree::list(f, "schemaElts");
        if !elements.is_empty() {
            let path = std::mem::replace(&mut self.catalog.search_path, vec![name]);
            let result = elements.iter().try_for_each(|e| self.run_element(e));
            self.catalog.search_path = path;
            result?;
        }
        Ok(())
    }

    fn run_element(&mut self, node: &Value) -> Result<()> {
        match tree::kind(node) {
            Some(("CreateStmt", f)) => self.create_table(f),
            Some(("ViewStmt", f)) => self.create_view(f),
            Some(("IndexStmt", f)) => self.create_index(f).map(|_| ()),
            Some(("CreateSeqStmt", f)) => self.create_sequence(f),
            _ => {
                self.not_previewed("This CREATE SCHEMA element");
                Ok(())
            }
        }
    }

    /// A type's schema and name, checked to be free.
    fn new_type_name(&self, names: &[Value], location: Option<usize>) -> Result<(String, String)> {
        let name =
            Name::from_list(names).ok_or_else(|| Fail::new("42601", "a type needs a name"))?;
        let schema = self.creation_schema(name.schema.as_deref(), location)?;
        if self.catalog.type_name_taken(&schema, &name.name) {
            return Err(
                Fail::new("42710", format!("type \"{}\" already exists", name.name)).at(location),
            );
        }
        Ok((schema, name.name))
    }

    pub fn create_enum(&mut self, f: &Value) -> Result<()> {
        let (schema, name) = self.new_type_name(tree::list(f, "typeName"), None)?;
        let labels = tree::strings(tree::list(f, "vals"));
        for (i, label) in labels.iter().enumerate() {
            if label.len() > 63 {
                return Err(
                    Fail::new("22023", format!("invalid enum label \"{label}\""))
                        .detail("Labels must be 63 bytes or less."),
                );
            }
            if labels.iter().take(i).any(|l| l == label) {
                return Err(Fail::new(
                    "42710",
                    format!("enum label \"{label}\" used more than once"),
                ));
            }
        }
        self.catalog.enums.push(EnumType {
            schema,
            name,
            labels,
            comment: None,
        });
        Ok(())
    }

    fn find_enum(&self, names: &[Value]) -> Result<usize> {
        let name = Name::from_list(names).unwrap_or(Name {
            schema: None,
            name: String::new(),
        });
        match self.catalog.find_type(name.schema.as_deref(), &name.name) {
            Some(Base::User {
                schema,
                name: n,
                kind: UserKind::Enum,
            }) => Ok(self
                .catalog
                .enums
                .iter()
                .position(|e| e.schema == schema && e.name == n)
                .ok_or_else(Fail::internal)?),
            Some(_) => Err(Fail::new(
                "42809",
                format!("\"{}\" is not an enum", display(&name)),
            )),
            None => Err(Fail::new(
                "42704",
                format!("type \"{}\" does not exist", display(&name)),
            )),
        }
    }

    pub fn alter_enum(&mut self, f: &Value) -> Result<()> {
        let index = self.find_enum(tree::list(f, "typeName"))?;
        let labels = &mut self.catalog.enum_mut(index)?.labels;
        let new = tree::str(f, "newVal").to_owned();
        if let Some(old) = tree::opt_str(f, "oldVal") {
            if labels.contains(&new) {
                return Err(Fail::new(
                    "42710",
                    format!("enum label \"{new}\" already exists"),
                ));
            }
            let at = labels.iter().position(|l| l == old).ok_or_else(|| {
                Fail::new("22023", format!("\"{old}\" is not an existing enum label"))
            })?;
            if let Some(label) = labels.get_mut(at) {
                *label = new;
            }
            return Ok(());
        }
        if labels.contains(&new) {
            if tree::bool(f, "skipIfNewValExists") {
                return Ok(());
            }
            return Err(Fail::new(
                "42710",
                format!("enum label \"{new}\" already exists"),
            ));
        }
        match tree::opt_str(f, "newValNeighbor") {
            Some(neighbor) => {
                let at = labels.iter().position(|l| l == neighbor).ok_or_else(|| {
                    Fail::new(
                        "22023",
                        format!("\"{neighbor}\" is not an existing enum label"),
                    )
                })?;
                let at = if tree::bool(f, "newValIsAfter") {
                    at + 1
                } else {
                    at
                };
                labels.insert(at, new);
            }
            None => labels.push(new),
        }
        Ok(())
    }

    pub fn create_domain(&mut self, f: &Value) -> Result<()> {
        let (schema, name) = self.new_type_name(tree::list(f, "domainname"), None)?;
        let base = resolve_type(
            &self.catalog,
            tree::field(f, "typeName").unwrap_or(&Value::Null),
        )?;
        let mut domain = Domain {
            schema: schema.clone(),
            name: name.clone(),
            base: base.clone(),
            not_null: false,
            default: None,
            checks: Vec::new(),
            comment: None,
        };
        for c in tree::list(f, "constraints") {
            let Some(c) = tree::as_kind(c, "Constraint") else {
                continue;
            };
            match tree::str(c, "contype") {
                "CONSTR_NOTNULL" => domain.not_null = true,
                "CONSTR_NULL" => domain.not_null = false,
                "CONSTR_DEFAULT" => {
                    let raw = tree::field(c, "raw_expr").unwrap_or(&Value::Null);
                    let scope = DomainScope {
                        catalog: &self.catalog,
                        value: None,
                    };
                    let e = Analyzer {
                        sql: self.sql,
                        scope: &scope,
                    }
                    .expr(raw)?;
                    domain.default = Some(expr::coerce_assignment(e, &self.catalog.base_of(&base)));
                }
                "CONSTR_CHECK" => {
                    let check = self.domain_check(&base, c)?;
                    let taken: Vec<String> = domain.checks.iter().map(|(n, _)| n.clone()).collect();
                    let check_name = match tree::opt_str(c, "conname") {
                        Some(n) => n.to_owned(),
                        None => choose_constraint_name(
                            &self.catalog,
                            &schema,
                            &name,
                            None,
                            "check",
                            &taken,
                        ),
                    };
                    domain.checks.push((check_name, check));
                }
                other => self.not_previewed(&format!(
                    "CREATE DOMAIN … {}",
                    other.strip_prefix("CONSTR_").unwrap_or(other)
                )),
            }
        }
        self.catalog.domains.push(domain);
        Ok(())
    }

    /// `ALTER DOMAIN … ADD CONSTRAINT`: a check, or NOT NULL.
    fn add_domain_constraint(
        &mut self,
        index: usize,
        schema: &str,
        domain: &str,
        base: &Ty,
        c: &Value,
    ) -> Result<()> {
        match tree::str(c, "contype") {
            "CONSTR_CHECK" => {}
            "CONSTR_NOTNULL" => {
                self.catalog.domain_mut(index)?.not_null = true;
                return Ok(());
            }
            other => {
                self.not_previewed(&format!(
                    "ALTER DOMAIN … ADD CONSTRAINT … {}",
                    other.strip_prefix("CONSTR_").unwrap_or(other)
                ));
                return Ok(());
            }
        }
        let check = self.domain_check(base, c)?;
        let taken: Vec<String> = self
            .catalog
            .domain_mut(index)?
            .checks
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        let check_name = match tree::opt_str(c, "conname") {
            Some(cn) => cn.to_owned(),
            None => choose_constraint_name(&self.catalog, schema, domain, None, "check", &taken),
        };
        self.catalog
            .domain_mut(index)?
            .checks
            .push((check_name, check));
        Ok(())
    }

    fn domain_check(&self, base: &Ty, c: &Value) -> Result<Expr> {
        let raw = tree::field(c, "raw_expr").unwrap_or(&Value::Null);
        let scope = DomainScope {
            catalog: &self.catalog,
            value: Some(self.catalog.base_of(base)),
        };
        Ok(Analyzer {
            sql: self.sql,
            scope: &scope,
        }
        .expr(raw)?)
    }

    pub fn alter_domain(&mut self, f: &Value) -> Result<()> {
        let name = Name::from_list(tree::list(f, "typeName")).unwrap_or(Name {
            schema: None,
            name: String::new(),
        });
        let Some(Base::User {
            schema,
            name: n,
            kind: UserKind::Domain,
        }) = self.catalog.find_type(name.schema.as_deref(), &name.name)
        else {
            return Err(Fail::new(
                "42704",
                format!("type \"{}\" does not exist", display(&name)),
            ));
        };
        let index = self
            .catalog
            .domains
            .iter()
            .position(|d| d.schema == schema && d.name == n)
            .ok_or_else(Fail::internal)?;
        let base = self.catalog.domain_mut(index)?.base.clone();
        match tree::str(f, "subtype") {
            "T" => {
                self.catalog.domain_mut(index)?.default = match tree::field(f, "def") {
                    Some(raw) => {
                        let scope = DomainScope {
                            catalog: &self.catalog,
                            value: None,
                        };
                        let e = Analyzer {
                            sql: self.sql,
                            scope: &scope,
                        }
                        .expr(raw)?;
                        Some(expr::coerce_assignment(e, &self.catalog.base_of(&base)))
                    }
                    None => None,
                };
            }
            "N" => self.catalog.domain_mut(index)?.not_null = false,
            "O" => self.catalog.domain_mut(index)?.not_null = true,
            "C" => {
                if let Some(c) = tree::field(f, "def").and_then(|d| tree::as_kind(d, "Constraint"))
                {
                    self.add_domain_constraint(index, &schema, &n, &base, c)?;
                }
            }
            "X" => {
                let drop = tree::str(f, "name");
                let checks = &mut self.catalog.domain_mut(index)?.checks;
                let before = checks.len();
                checks.retain(|(cn, _)| cn != drop);
                if checks.len() == before && !tree::bool(f, "missing_ok") {
                    return Err(Fail::new(
                        "42704",
                        format!("constraint \"{drop}\" of domain \"{n}\" does not exist"),
                    ));
                }
            }
            // VALIDATE CONSTRAINT: the model has no NOT VALID on domains,
            // only the name is checked.
            "V" => {
                let name = tree::str(f, "name");
                if !self
                    .catalog
                    .domain_mut(index)?
                    .checks
                    .iter()
                    .any(|(cn, _)| cn == name)
                {
                    return Err(Fail::new(
                        "42704",
                        format!("constraint \"{name}\" of domain \"{n}\" does not exist"),
                    ));
                }
            }
            _ => {
                let what = format!("This {} action", self.statement_label());
                self.not_previewed(&what);
            }
        }
        Ok(())
    }

    pub fn create_composite(&mut self, f: &Value) -> Result<()> {
        let rv = tree::field(f, "typevar").unwrap_or(&Value::Null);
        let name = Name::from_range_var(rv);
        let schema = self.creation_schema(name.schema.as_deref(), tree::location(rv))?;
        if self.catalog.type_name_taken(&schema, &name.name)
            || self.catalog.relation_name_taken(&schema, &name.name)
        {
            return Err(
                Fail::new("42710", format!("type \"{}\" already exists", name.name))
                    .at(tree::location(rv)),
            );
        }
        let mut attributes = Vec::new();
        for c in tree::list(f, "coldeflist") {
            let Some(c) = tree::as_kind(c, "ColumnDef") else {
                continue;
            };
            let ty = resolve_type(
                &self.catalog,
                tree::field(c, "typeName").unwrap_or(&Value::Null),
            )?;
            attributes.push((tree::str(c, "colname").to_owned(), ty));
        }
        self.catalog.composites.push(Composite {
            schema,
            name: name.name,
            attributes,
            comment: None,
        });
        Ok(())
    }

    pub fn create_range(&mut self, f: &Value) -> Result<()> {
        let (schema, name) = self.new_type_name(tree::list(f, "typeName"), None)?;
        self.catalog.ranges.push((schema, name));
        Ok(())
    }

    // ---- Views -------------------------------------------------------------------

    pub fn create_view(&mut self, f: &Value) -> Result<()> {
        let rv = tree::field(f, "view").unwrap_or(&Value::Null);
        let select = tree::field(f, "query").unwrap_or(&Value::Null);
        let aliases = tree::strings(tree::list(f, "aliases"));
        self.view(
            rv,
            select,
            &aliases,
            RelKind::View,
            tree::bool(f, "replace"),
            false,
        )
    }

    pub fn create_matview(&mut self, f: &Value) -> Result<()> {
        let into = tree::field(f, "into").unwrap_or(&Value::Null);
        let rv = tree::field(into, "rel").unwrap_or(&Value::Null);
        let select = tree::field(f, "query").unwrap_or(&Value::Null);
        let aliases = tree::strings(tree::list(into, "colNames"));
        self.view(
            rv,
            select,
            &aliases,
            RelKind::MaterializedView,
            false,
            tree::bool(f, "if_not_exists"),
        )
    }

    fn view(
        &mut self,
        rv: &Value,
        select: &Value,
        aliases: &[String],
        kind: RelKind,
        replace: bool,
        if_not_exists: bool,
    ) -> Result<()> {
        let name = Name::from_range_var(rv);
        let location = tree::location(rv);
        let schema = self.creation_schema(name.schema.as_deref(), location)?;
        let existing = self
            .catalog
            .find_rel(Some(&schema), &name.name)
            .map(|r| (r.id, r.kind));
        let replacing = match existing {
            Some((id, RelKind::View)) if replace && kind == RelKind::View => Some(id),
            Some(_) if if_not_exists => return Ok(()),
            Some(_) => {
                return Err(Fail::new(
                    "42P07",
                    format!("relation \"{}\" already exists", name.name),
                )
                .at(location));
            }
            None if self.catalog.relation_name_taken(&schema, &name.name) => {
                return Err(Fail::new(
                    "42P07",
                    format!("relation \"{}\" already exists", name.name),
                )
                .at(location));
            }
            None => None,
        };
        let mut query = view::analyse(&self.catalog, self.sql, select)?;
        for (target, alias) in query.targets.iter_mut().zip(aliases) {
            target.name.clone_from(alias);
        }
        let outputs = view::output_columns(&query);
        for (i, (n, _)) in outputs.iter().enumerate() {
            if outputs.iter().take(i).any(|(m, _)| m == n) {
                return Err(Fail::new(
                    "42701",
                    format!("column \"{n}\" specified more than once"),
                ));
            }
        }
        if query.raw.is_some() {
            self.note(Diagnostic::info(
                Code::NotRun,
                "This view's query is kept as written: its columns are not modelled in this preview.",
            ));
        }
        let columns: Vec<Column> = outputs
            .into_iter()
            .map(|(name, ty)| Column {
                id: self.catalog.next_id(),
                name,
                ty: if ty.is_unknown() {
                    Ty::builtin("text")
                } else {
                    ty
                },
                not_null: false,
                default: None,
                identity: None,
                generated: false,
                comment: None,
            })
            .collect();
        if let Some(id) = replacing {
            let rel = self.catalog.get_mut(id)?;
            rel.columns = columns;
            rel.view = Some(query);
            return Ok(());
        }
        let id = self.catalog.next_id();
        self.catalog.rels.push(Rel {
            id,
            schema,
            name: name.name,
            kind,
            columns,
            constraints: Vec::new(),
            indexes: Vec::new(),
            policies: Vec::new(),
            comment: None,
            rls: false,
            rls_forced: false,
            unlogged: false,
            partition_key: None,
            partition_of: None,
            view: Some(query),
        });
        Ok(())
    }

    // ---- Policies -------------------------------------------------------------------

    fn policy_expr(&self, rel: u32, raw: Option<&Value>) -> Result<Option<Expr>> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        let scope = TableScope {
            catalog: &self.catalog,
            rel,
            columns: true,
            generated: &[],
        };
        let e = Analyzer {
            sql: self.sql,
            scope: &scope,
        }
        .expr(raw)?;
        Ok(Some(expr::resolve_unknown(e, &Ty::builtin("bool"))))
    }

    fn roles(roles: &[Value]) -> Vec<String> {
        roles
            .iter()
            .filter_map(|r| tree::as_kind(r, "RoleSpec"))
            .filter(|r| tree::str(r, "roletype") != "ROLESPEC_PUBLIC")
            .map(|r| tree::str(r, "rolename").to_owned())
            .collect()
    }

    pub fn create_policy(&mut self, f: &Value) -> Result<()> {
        let table = tree::field(f, "table").unwrap_or(&Value::Null);
        let rel = self.rel_of(table)?;
        let name = tree::str(f, "policy_name").to_owned();
        let r = self.catalog.get(rel)?;
        if r.policies.iter().any(|p| p.name == name) {
            return Err(Fail::new(
                "42710",
                format!("policy \"{name}\" for table \"{}\" already exists", r.name),
            ));
        }
        let command = match tree::str(f, "cmd_name") {
            "select" => PolicyCommand::Select,
            "insert" => PolicyCommand::Insert,
            "update" => PolicyCommand::Update,
            "delete" => PolicyCommand::Delete,
            _ => PolicyCommand::All,
        };
        let using = self.policy_expr(rel, tree::field(f, "qual"))?;
        let check = self.policy_expr(rel, tree::field(f, "with_check"))?;
        if command == PolicyCommand::Insert && using.is_some() {
            return Err(Fail::new(
                "42601",
                "only WITH CHECK expression allowed for INSERT",
            ));
        }
        if matches!(command, PolicyCommand::Select | PolicyCommand::Delete) && check.is_some() {
            return Err(Fail::new(
                "42601",
                "WITH CHECK cannot be applied to SELECT or DELETE",
            ));
        }
        self.catalog.get_mut(rel)?.policies.push(Policy {
            name,
            command,
            permissive: tree::bool(f, "permissive"),
            roles: Self::roles(tree::list(f, "roles")),
            using,
            check,
        });
        Ok(())
    }

    pub fn alter_policy(&mut self, f: &Value) -> Result<()> {
        let table = tree::field(f, "table").unwrap_or(&Value::Null);
        let rel = self.rel_of(table)?;
        let name = tree::str(f, "policy_name");
        let using = self.policy_expr(rel, tree::field(f, "qual"))?;
        let check = self.policy_expr(rel, tree::field(f, "with_check"))?;
        let roles = tree::list(f, "roles");
        let r = self.catalog.get_mut(rel)?;
        let table_name = r.name.clone();
        let policy = r
            .policies
            .iter_mut()
            .find(|p| p.name == name)
            .ok_or_else(|| {
                Fail::new(
                    "42704",
                    format!("policy \"{name}\" for table \"{table_name}\" does not exist"),
                )
            })?;
        if using.is_some() {
            policy.using = using;
        }
        if check.is_some() {
            policy.check = check;
        }
        if !roles.is_empty() {
            policy.roles = Self::roles(roles);
        }
        Ok(())
    }

    // ---- Comments -------------------------------------------------------------------

    /// `COMMENT ON TABLE|VIEW|COLUMN|TYPE|DOMAIN|CONSTRAINT …`.
    #[allow(clippy::too_many_lines)] // one arm per object kind
    pub fn comment(&mut self, f: &Value) -> Result<()> {
        let comment = tree::opt_str(f, "comment").map(str::to_owned);
        let object = tree::field(f, "object").unwrap_or(&Value::Null);
        let names = tree::strings(tree::list_items(object));
        match tree::str(f, "objtype") {
            objtype @ ("OBJECT_TABLE" | "OBJECT_VIEW" | "OBJECT_MATVIEW") => {
                let name = Name::from_list(tree::list_items(object)).unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                let rel = self.named_rel(&name)?;
                let r = self.catalog.get_mut(rel)?;
                let wanted = match objtype {
                    "OBJECT_TABLE" => r.kind.is_table(),
                    "OBJECT_VIEW" => r.kind == RelKind::View,
                    _ => r.kind == RelKind::MaterializedView,
                };
                if !wanted {
                    let word = match objtype {
                        "OBJECT_TABLE" => "a table",
                        "OBJECT_VIEW" => "a view",
                        _ => "a materialized view",
                    };
                    return Err(Fail::new("42809", format!("\"{}\" is not {word}", r.name)));
                }
                r.comment = comment;
            }
            "OBJECT_COLUMN" => {
                let Some((column, table)) = names.split_last() else {
                    return Ok(());
                };
                let name = Name::from_list(
                    tree::list_items(object)
                        .get(..table.len())
                        .unwrap_or_default(),
                )
                .unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                let rel = self.named_rel(&name)?;
                let r = self.catalog.get_mut(rel)?;
                let table_name = r.name.clone();
                let c = r
                    .columns
                    .iter_mut()
                    .find(|c| &c.name == column)
                    .ok_or_else(|| {
                        Fail::new(
                            "42703",
                            format!(
                                "column \"{column}\" of relation \"{table_name}\" does not exist"
                            ),
                        )
                    })?;
                c.comment = comment;
            }
            "OBJECT_TABCONSTRAINT" => {
                let Some((constraint, table)) = names.split_last() else {
                    return Ok(());
                };
                let name = Name::from_list(
                    tree::list_items(object)
                        .get(..table.len())
                        .unwrap_or_default(),
                )
                .unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                let rel = self.named_rel(&name)?;
                let r = self.catalog.get_mut(rel)?;
                let table_name = r.name.clone();
                let c = r
                    .constraints
                    .iter_mut()
                    .find(|c| &c.name == constraint)
                    .ok_or_else(|| {
                        Fail::new(
                            "42704",
                            format!("constraint \"{constraint}\" for table \"{table_name}\" does not exist"),
                        )
                    })?;
                c.comment = comment;
            }
            "OBJECT_TYPE" | "OBJECT_DOMAIN" => {
                let names = tree::as_kind(object, "TypeName").map_or_else(
                    || tree::list_items(object).to_vec(),
                    |t| tree::list(t, "names").to_vec(),
                );
                let name = Name::from_list(&names).unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                match self.catalog.find_type(name.schema.as_deref(), &name.name) {
                    Some(Base::User {
                        schema,
                        name: n,
                        kind,
                    }) => match kind {
                        UserKind::Enum => {
                            if let Some(e) = self
                                .catalog
                                .enums
                                .iter_mut()
                                .find(|e| e.schema == schema && e.name == n)
                            {
                                e.comment = comment;
                            }
                        }
                        UserKind::Domain => {
                            if let Some(d) = self
                                .catalog
                                .domains
                                .iter_mut()
                                .find(|d| d.schema == schema && d.name == n)
                            {
                                d.comment = comment;
                            }
                        }
                        UserKind::Composite => {
                            if let Some(c) = self
                                .catalog
                                .composites
                                .iter_mut()
                                .find(|c| c.schema == schema && c.name == n)
                            {
                                c.comment = comment;
                            }
                        }
                        UserKind::Range | UserKind::Extension => {}
                    },
                    _ => {
                        return Err(Fail::new(
                            "42704",
                            format!("type \"{}\" does not exist", display(&name)),
                        ));
                    }
                }
            }
            // Not in the model, not checked here.
            _ => {
                let what = self.statement_label();
                self.not_previewed(&what);
            }
        }
        Ok(())
    }

    /// A relation by name, or Postgres's error.
    fn named_rel(&self, name: &Name) -> Result<u32> {
        self.catalog
            .find_rel(name.schema.as_deref(), &name.name)
            .map(|r| r.id)
            .ok_or_else(|| {
                Fail::new(
                    "42P01",
                    format!("relation \"{}\" does not exist", display(name)),
                )
            })
    }

    // ---- Renames --------------------------------------------------------------------

    #[allow(clippy::too_many_lines)] // one arm per kind of object
    pub fn rename(&mut self, f: &Value) -> Result<()> {
        let new = tree::str(f, "newname").to_owned();
        let sub = tree::str(f, "subname").to_owned();
        let relation = tree::field(f, "relation");
        match tree::str(f, "renameType") {
            "OBJECT_TABLE" | "OBJECT_VIEW" | "OBJECT_MATVIEW" | "OBJECT_FOREIGN_TABLE" => {
                let rv = relation.unwrap_or(&Value::Null);
                let rel = match self.rel_of(rv) {
                    Ok(rel) => rel,
                    Err(_) if tree::bool(f, "missing_ok") => return Ok(()),
                    Err(e) => return Err(e),
                };
                let schema = self.catalog.get(rel)?.schema.clone();
                if self.catalog.relation_name_taken(&schema, &new)
                    || self.catalog.type_name_taken(&schema, &new)
                {
                    return Err(Fail::new(
                        "42P07",
                        format!("relation \"{new}\" already exists"),
                    ));
                }
                self.catalog.get_mut(rel)?.name = new;
            }
            "OBJECT_INDEX" => {
                let rv = relation.unwrap_or(&Value::Null);
                let name = Name::from_range_var(rv);
                let found = self.catalog.rels.iter().find_map(|r| {
                    let in_schema = name.schema.as_ref().is_none_or(|s| *s == r.schema)
                        && (name.schema.is_some() || self.catalog.path().any(|p| *p == r.schema));
                    if !in_schema {
                        return None;
                    }
                    if r.indexes.iter().any(|i| i.name == name.name) {
                        return Some((r.id, false));
                    }
                    r.constraints
                        .iter()
                        .any(|c| {
                            c.name == name.name
                                && matches!(
                                    c.kind,
                                    ConstraintKind::PrimaryKey
                                        | ConstraintKind::Unique
                                        | ConstraintKind::Exclusion
                                )
                        })
                        .then_some((r.id, true))
                });
                let Some((rel, constraint)) = found else {
                    if tree::bool(f, "missing_ok") {
                        return Ok(());
                    }
                    return Err(Fail::new(
                        "42P01",
                        format!("relation \"{}\" does not exist", display(&name)),
                    )
                    .at(tree::location(rv)));
                };
                let schema = self.catalog.get(rel)?.schema.clone();
                if self.catalog.relation_name_taken(&schema, &new) {
                    return Err(Fail::new(
                        "42P07",
                        format!("relation \"{new}\" already exists"),
                    ));
                }
                let r = self.catalog.get_mut(rel)?;
                if constraint {
                    if let Some(c) = r.constraints.iter_mut().find(|c| c.name == name.name) {
                        c.name = new;
                    }
                } else if let Some(i) = r.indexes.iter_mut().find(|i| i.name == name.name) {
                    i.name = new;
                }
            }
            "OBJECT_SEQUENCE" => {
                let name = Name::from_range_var(relation.unwrap_or(&Value::Null));
                if let Some(s) = self
                    .catalog
                    .sequences
                    .iter_mut()
                    .find(|(_, n)| *n == name.name)
                {
                    s.1 = new;
                }
            }
            "OBJECT_COLUMN" | "OBJECT_ATTRIBUTE"
                if tree::str(f, "relationType") == "OBJECT_TYPE" =>
            {
                let name = Name::from_range_var(relation.unwrap_or(&Value::Null));
                if let Some(c) = self.catalog.composites.iter_mut().find(|c| {
                    c.name == name.name && name.schema.as_ref().is_none_or(|s| *s == c.schema)
                }) && let Some(a) = c.attributes.iter_mut().find(|(n, _)| *n == sub)
                {
                    a.0 = new;
                }
            }
            "OBJECT_COLUMN" => {
                let rv = relation.unwrap_or(&Value::Null);
                let rel = self.rel_of(rv)?;
                let r = self.catalog.get_mut(rel)?;
                let table = r.name.clone();
                if r.columns.iter().any(|c| c.name == new) {
                    return Err(Fail::new(
                        "42701",
                        format!("column \"{new}\" of relation \"{table}\" already exists"),
                    ));
                }
                let c = r
                    .columns
                    .iter_mut()
                    .find(|c| c.name == sub)
                    .ok_or_else(|| {
                        Fail::new("42703", format!("column \"{sub}\" does not exist"))
                    })?;
                c.name = new;
            }
            "OBJECT_TABCONSTRAINT" => {
                let rv = relation.unwrap_or(&Value::Null);
                let rel = self.rel_of(rv)?;
                let r = self.catalog.get_mut(rel)?;
                let table = r.name.clone();
                if r.constraints.iter().any(|c| c.name == new) {
                    return Err(Fail::new(
                        "42710",
                        format!("constraint \"{new}\" for relation \"{table}\" already exists"),
                    ));
                }
                let c = r
                    .constraints
                    .iter_mut()
                    .find(|c| c.name == sub)
                    .ok_or_else(|| {
                        Fail::new(
                            "42704",
                            format!("constraint \"{sub}\" for table \"{table}\" does not exist"),
                        )
                    })?;
                c.name = new;
            }
            "OBJECT_POLICY" => {
                let rv = relation.unwrap_or(&Value::Null);
                let rel = self.rel_of(rv)?;
                let r = self.catalog.get_mut(rel)?;
                let table = r.name.clone();
                let p = r
                    .policies
                    .iter_mut()
                    .find(|p| p.name == sub)
                    .ok_or_else(|| {
                        Fail::new(
                            "42704",
                            format!("policy \"{sub}\" for table \"{table}\" does not exist"),
                        )
                    })?;
                p.name = new;
            }
            "OBJECT_TYPE" | "OBJECT_DOMAIN" => {
                let object = tree::field(f, "object").unwrap_or(&Value::Null);
                let name = Name::from_list(tree::list_items(object)).unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                let Some(Base::User {
                    schema, name: old, ..
                }) = self.catalog.find_type(name.schema.as_deref(), &name.name)
                else {
                    return Err(Fail::new(
                        "42704",
                        format!("type \"{}\" does not exist", display(&name)),
                    ));
                };
                if self.catalog.type_name_taken(&schema, &new) {
                    return Err(Fail::new("42710", format!("type \"{new}\" already exists")));
                }
                self.rename_type(&schema, &old, &schema, &new);
            }
            "OBJECT_DOMCONSTRAINT" => {
                let object = tree::field(f, "object").unwrap_or(&Value::Null);
                let name = Name::from_list(tree::list_items(object)).unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                let Some(Base::User {
                    schema,
                    name: n,
                    kind: UserKind::Domain,
                }) = self.catalog.find_type(name.schema.as_deref(), &name.name)
                else {
                    return Err(Fail::new(
                        "42704",
                        format!("type \"{}\" does not exist", display(&name)),
                    ));
                };
                let index = self
                    .catalog
                    .domains
                    .iter()
                    .position(|d| d.schema == schema && d.name == n)
                    .ok_or_else(Fail::internal)?;
                let domain = remus_core::emit::ident::quote_ident(&n);
                let checks = &mut self.catalog.domain_mut(index)?.checks;
                if !checks.iter().any(|(cn, _)| *cn == sub) {
                    return Err(Fail::new(
                        "42704",
                        format!("constraint \"{sub}\" for domain {domain} does not exist"),
                    ));
                }
                if checks.iter().any(|(cn, _)| *cn == new) {
                    return Err(Fail::new(
                        "42710",
                        format!("constraint \"{new}\" for domain {domain} already exists"),
                    ));
                }
                if let Some(check) = checks.iter_mut().find(|(cn, _)| *cn == sub) {
                    check.0 = new;
                }
            }
            "OBJECT_SCHEMA" => {
                if !self.catalog.schemas.contains(&sub) {
                    return Err(Fail::new(
                        "3F000",
                        format!("schema \"{sub}\" does not exist"),
                    ));
                }
                if self.catalog.schemas.contains(&new) {
                    return Err(Fail::new(
                        "42P06",
                        format!("schema \"{new}\" already exists"),
                    ));
                }
                self.move_schema(&sub, &new);
            }
            // Not in the model, not checked here.
            _ => {
                let what = format!("{} … RENAME", self.statement_label());
                self.not_previewed(&what);
            }
        }
        Ok(())
    }

    /// Every use of a type follows its new name (columns, attributes,
    /// domains, the types expressions carry).
    fn rename_type(&mut self, schema: &str, old: &str, new_schema: &str, new: &str) {
        let mut fix = |ty: &mut Ty| {
            if let Base::User {
                schema: s, name: n, ..
            } = &mut ty.base
                && s == schema
                && n == old
            {
                new_schema.clone_into(s);
                new.clone_into(n);
            }
        };
        let c = &mut self.catalog;
        for e in &mut c.enums {
            if e.schema == schema && e.name == old {
                new_schema.clone_into(&mut e.schema);
                new.clone_into(&mut e.name);
            }
        }
        for d in &mut c.domains {
            if d.schema == schema && d.name == old {
                new_schema.clone_into(&mut d.schema);
                new.clone_into(&mut d.name);
            }
            fix(&mut d.base);
            d.default.iter_mut().for_each(|e| e.types_mut(&mut fix));
            d.checks.iter_mut().for_each(|(_, e)| e.types_mut(&mut fix));
        }
        for t in &mut c.composites {
            if t.schema == schema && t.name == old {
                new_schema.clone_into(&mut t.schema);
                new.clone_into(&mut t.name);
            }
            t.attributes.iter_mut().for_each(|(_, ty)| fix(ty));
        }
        for r in &mut c.ranges {
            if r.0 == schema && r.1 == old {
                new_schema.clone_into(&mut r.0);
                new.clone_into(&mut r.1);
            }
        }
        for rel in &mut c.rels {
            for col in &mut rel.columns {
                fix(&mut col.ty);
                col.default.iter_mut().for_each(|e| e.types_mut(&mut fix));
            }
            for con in &mut rel.constraints {
                con.check.iter_mut().for_each(|e| e.types_mut(&mut fix));
            }
            for p in &mut rel.policies {
                p.using
                    .iter_mut()
                    .chain(&mut p.check)
                    .for_each(|e| e.types_mut(&mut fix));
            }
        }
    }

    fn move_schema(&mut self, old: &str, new: &str) {
        let types: Vec<String> = self
            .catalog
            .enums
            .iter()
            .filter(|e| e.schema == old)
            .map(|e| e.name.clone())
            .chain(
                self.catalog
                    .domains
                    .iter()
                    .filter(|d| d.schema == old)
                    .map(|d| d.name.clone()),
            )
            .chain(
                self.catalog
                    .composites
                    .iter()
                    .filter(|d| d.schema == old)
                    .map(|d| d.name.clone()),
            )
            .collect();
        for t in types {
            self.rename_type(old, &t, new, &t);
        }
        for r in &mut self.catalog.rels {
            if r.schema == old {
                new.clone_into(&mut r.schema);
            }
        }
        for s in &mut self.catalog.sequences {
            if s.0 == old {
                new.clone_into(&mut s.0);
            }
        }
        for e in &mut self.catalog.extensions {
            if e.schema == old {
                new.clone_into(&mut e.schema);
            }
        }
        for s in &mut self.catalog.schemas {
            if s == old {
                new.clone_into(s);
            }
        }
    }

    /// `ALTER … SET SCHEMA`.
    pub fn set_schema(&mut self, f: &Value) -> Result<()> {
        let target = tree::str(f, "newschema").to_owned();
        if !self.catalog.schemas.contains(&target) {
            return Err(Fail::new(
                "3F000",
                format!("schema \"{target}\" does not exist"),
            ));
        }
        match tree::str(f, "objectType") {
            "OBJECT_TABLE" | "OBJECT_VIEW" | "OBJECT_MATVIEW" => {
                let rel = self.rel_of(tree::field(f, "relation").unwrap_or(&Value::Null))?;
                let name = self.catalog.get(rel)?.name.clone();
                if self.catalog.relation_name_taken(&target, &name) {
                    return Err(Fail::new(
                        "42P07",
                        format!("relation \"{name}\" already exists in schema \"{target}\""),
                    ));
                }
                self.catalog.get_mut(rel)?.schema = target;
            }
            "OBJECT_TYPE" | "OBJECT_DOMAIN" => {
                let object = tree::field(f, "object").unwrap_or(&Value::Null);
                let name = Name::from_list(tree::list_items(object)).unwrap_or(Name {
                    schema: None,
                    name: String::new(),
                });
                let Some(Base::User {
                    schema, name: n, ..
                }) = self.catalog.find_type(name.schema.as_deref(), &name.name)
                else {
                    return Err(Fail::new(
                        "42704",
                        format!("type \"{}\" does not exist", display(&name)),
                    ));
                };
                self.rename_type(&schema, &n, &target, &n);
            }
            "OBJECT_EXTENSION" => {
                let name = tree::field(f, "object")
                    .and_then(tree::string)
                    .unwrap_or("");
                if let Some(e) = self.catalog.extensions.iter_mut().find(|e| e.name == name) {
                    e.schema = target;
                }
            }
            // Not in the model, not checked here.
            _ => {
                let what = format!("{} … SET SCHEMA", self.statement_label());
                self.not_previewed(&what);
            }
        }
        Ok(())
    }

    // ---- DROP -----------------------------------------------------------------------

    #[allow(clippy::too_many_lines)] // one arm per kind of object
    pub fn drop(&mut self, f: &Value) -> Result<()> {
        let cascade = tree::str(f, "behavior") == "DROP_CASCADE";
        let missing_ok = tree::bool(f, "missing_ok");
        let objects = tree::list(f, "objects");
        match tree::str(f, "removeType") {
            kind @ ("OBJECT_TABLE" | "OBJECT_VIEW" | "OBJECT_MATVIEW") => {
                let (word, wanted): (&str, fn(RelKind) -> bool) = match kind {
                    "OBJECT_TABLE" => ("table", RelKind::is_table),
                    "OBJECT_VIEW" => ("view", |k| k == RelKind::View),
                    _ => ("materialized view", |k| k == RelKind::MaterializedView),
                };
                let mut ids = Vec::new();
                for o in objects {
                    let name = Name::from_list(tree::list_items(o)).unwrap_or(Name {
                        schema: None,
                        name: String::new(),
                    });
                    match self.catalog.find_rel(name.schema.as_deref(), &name.name) {
                        Some(r) if wanted(r.kind) => ids.push(r.id),
                        Some(r) => {
                            let hint = match r.kind {
                                RelKind::View => "Use DROP VIEW to remove a view.",
                                RelKind::MaterializedView => {
                                    "Use DROP MATERIALIZED VIEW to remove a materialized view."
                                }
                                _ => "Use DROP TABLE to remove a table.",
                            };
                            return Err(Fail::new(
                                "42809",
                                format!("\"{}\" is not a {word}", r.name),
                            )
                            .hint(hint));
                        }
                        None if missing_ok => {}
                        None => {
                            return Err(Fail::new(
                                "42P01",
                                format!("{word} \"{}\" does not exist", display(&name)),
                            ));
                        }
                    }
                }
                self.drop_rels(&ids, cascade, word)?;
            }
            "OBJECT_INDEX" => {
                for o in objects {
                    let name = Name::from_list(tree::list_items(o)).unwrap_or(Name {
                        schema: None,
                        name: String::new(),
                    });
                    let found = self.catalog.rels.iter().find_map(|r| {
                        let visible = name.schema.as_ref().map_or_else(
                            || self.catalog.path().any(|p| *p == r.schema),
                            |s| *s == r.schema,
                        );
                        if !visible {
                            return None;
                        }
                        if r.indexes.iter().any(|i| i.name == name.name) {
                            return Some((r.id, None));
                        }
                        r.constraints
                            .iter()
                            .find(|c| {
                                c.name == name.name
                                    && matches!(
                                        c.kind,
                                        ConstraintKind::PrimaryKey
                                            | ConstraintKind::Unique
                                            | ConstraintKind::Exclusion
                                    )
                            })
                            .map(|c| (r.id, Some((c.name.clone(), r.name.clone()))))
                    });
                    match found {
                        Some((_, Some((constraint, table)))) => {
                            return Err(Fail::new(
                                "2BP01",
                                format!("cannot drop index {} because constraint {constraint} on table {table} requires it", name.name),
                            )
                            .hint(format!("You can drop constraint {constraint} on table {table} instead.")));
                        }
                        Some((rel, None)) => {
                            self.catalog
                                .get_mut(rel)?
                                .indexes
                                .retain(|i| i.name != name.name);
                        }
                        None if missing_ok => {}
                        None => {
                            return Err(Fail::new(
                                "42704",
                                format!("index \"{}\" does not exist", display(&name)),
                            ));
                        }
                    }
                }
            }
            "OBJECT_TYPE" | "OBJECT_DOMAIN" => {
                for o in objects {
                    let names = tree::as_kind(o, "TypeName").map_or_else(
                        || tree::list_items(o).to_vec(),
                        |t| tree::list(t, "names").to_vec(),
                    );
                    let name = Name::from_list(&names).unwrap_or(Name {
                        schema: None,
                        name: String::new(),
                    });
                    let Some(Base::User {
                        schema,
                        name: n,
                        kind,
                    }) = self.catalog.find_type(name.schema.as_deref(), &name.name)
                    else {
                        if missing_ok {
                            continue;
                        }
                        return Err(Fail::new(
                            "42704",
                            format!("type \"{}\" does not exist", display(&name)),
                        ));
                    };
                    if kind == UserKind::Extension {
                        return Err(Fail::new(
                            "2BP01",
                            format!(
                                "cannot drop type {n} because extension that provides it requires it"
                            ),
                        ));
                    }
                    self.drop_type(&schema, &n, cascade)?;
                }
            }
            "OBJECT_SCHEMA" => {
                for o in objects {
                    let name = tree::string(o).unwrap_or("").to_owned();
                    if !self.catalog.schemas.contains(&name) {
                        if missing_ok {
                            continue;
                        }
                        return Err(Fail::new(
                            "3F000",
                            format!("schema \"{name}\" does not exist"),
                        ));
                    }
                    self.drop_schema(&name, cascade)?;
                }
            }
            "OBJECT_EXTENSION" => {
                for o in objects {
                    let name = tree::string(o).unwrap_or("").to_owned();
                    let Some(ext) = self
                        .catalog
                        .extensions
                        .iter()
                        .find(|e| e.name == name)
                        .cloned()
                    else {
                        if missing_ok {
                            continue;
                        }
                        return Err(Fail::new(
                            "42704",
                            format!("extension \"{name}\" does not exist"),
                        ));
                    };
                    let provided = types::extension(&name).map(|e| e.types).unwrap_or_default();
                    for t in provided {
                        self.drop_type(&ext.schema, t, cascade)?;
                    }
                    self.catalog.extensions.retain(|e| e.name != name);
                }
            }
            "OBJECT_POLICY" => {
                for o in objects {
                    let mut items = tree::list_items(o).to_vec();
                    let Some(policy) = items
                        .pop()
                        .as_ref()
                        .and_then(tree::string)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let name = Name::from_list(&items).unwrap_or(Name {
                        schema: None,
                        name: String::new(),
                    });
                    let rel = match self.named_rel(&name) {
                        Ok(rel) => rel,
                        Err(_) if missing_ok => continue,
                        Err(e) => return Err(e),
                    };
                    let r = self.catalog.get_mut(rel)?;
                    let before = r.policies.len();
                    r.policies.retain(|p| p.name != policy);
                    if r.policies.len() == before && !missing_ok {
                        return Err(Fail::new(
                            "42704",
                            format!(
                                "policy \"{policy}\" for table \"{}\" does not exist",
                                r.name
                            ),
                        ));
                    }
                }
            }
            "OBJECT_SEQUENCE" => {
                for o in objects {
                    let name = Name::from_list(tree::list_items(o)).unwrap_or(Name {
                        schema: None,
                        name: String::new(),
                    });
                    self.catalog.sequences.retain(|(_, n)| *n != name.name);
                }
            }
            // Not in the model, not checked here.
            _ => {
                let what = self.statement_label();
                self.not_previewed(&what);
            }
        }
        Ok(())
    }

    /// Drops relations, with what depends on them: their partitions
    /// always, foreign keys and views pointing at them with `CASCADE`.
    fn drop_rels(&mut self, ids: &[u32], cascade: bool, word: &str) -> Result<()> {
        let mut doomed: Vec<u32> = ids.to_vec();
        // Partitions go with their parent.
        loop {
            let more: Vec<u32> = self
                .catalog
                .rels
                .iter()
                .filter(|r| {
                    r.partition_of
                        .as_ref()
                        .is_some_and(|(p, _)| doomed.contains(p))
                        && !doomed.contains(&r.id)
                })
                .map(|r| r.id)
                .collect();
            if more.is_empty() {
                break;
            }
            doomed.extend(more);
        }
        let mut details = Vec::new();
        let mut foreign: Vec<(u32, String)> = Vec::new();
        loop {
            let mut added = false;
            for r in &self.catalog.rels {
                if doomed.contains(&r.id) {
                    continue;
                }
                if let Some(q) = &r.view
                    && let Some(reads) = q.relations().into_iter().find(|x| doomed.contains(x))
                {
                    let read = self
                        .catalog
                        .rel(reads)
                        .map(|x| x.name.clone())
                        .unwrap_or_default();
                    details.push(format!(
                        "{} {} depends on {} {read}",
                        r.kind.word(),
                        r.name,
                        self.catalog.rel(reads).map_or("table", |x| x.kind.word())
                    ));
                    doomed.push(r.id);
                    added = true;
                    break;
                }
            }
            if !added {
                break;
            }
        }
        for r in &self.catalog.rels {
            if doomed.contains(&r.id) {
                continue;
            }
            for c in &r.constraints {
                if let Some(fk) = &c.foreign
                    && doomed.contains(&fk.target)
                {
                    let target = self
                        .catalog
                        .rel(fk.target)
                        .map(|x| x.name.clone())
                        .unwrap_or_default();
                    details.push(format!(
                        "constraint {} on table {} depends on table {target}",
                        c.name, r.name
                    ));
                    foreign.push((r.id, c.name.clone()));
                }
            }
        }
        if !details.is_empty() && !cascade {
            let first = ids
                .first()
                .and_then(|id| self.catalog.rel(*id))
                .map(|r| r.name.clone())
                .unwrap_or_default();
            return Err(Fail::new(
                "2BP01",
                format!("cannot drop {word} {first} because other objects depend on it"),
            )
            .detail(details.join("\n"))
            .hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }
        for (rel, name) in foreign {
            if let Some(r) = self.catalog.rel_mut(rel) {
                r.constraints.retain(|c| c.name != name);
            }
        }
        self.catalog.rels.retain(|r| !doomed.contains(&r.id));
        Ok(())
    }

    fn drop_type(&mut self, schema: &str, name: &str, cascade: bool) -> Result<()> {
        let uses = |ty: &Ty| matches!(&ty.base, Base::User { schema: s, name: n, .. } if s == schema && n == name);
        let mut dependents = Vec::new();
        let mut columns: Vec<(u32, u32)> = Vec::new();
        for r in &self.catalog.rels {
            for c in &r.columns {
                if uses(&c.ty) {
                    dependents.push(format!(
                        "column {} of {} {} depends on type {name}",
                        c.name,
                        r.kind.word(),
                        r.name
                    ));
                    columns.push((r.id, c.id));
                }
            }
        }
        for d in &self.catalog.domains {
            if uses(&d.base) {
                dependents.push(format!("type {} depends on type {name}", d.name));
            }
        }
        for t in &self.catalog.composites {
            if t.attributes.iter().any(|(_, ty)| uses(ty)) {
                dependents.push(format!(
                    "column of composite type {} depends on type {name}",
                    t.name
                ));
            }
        }
        if !dependents.is_empty() && !cascade {
            return Err(Fail::new(
                "2BP01",
                format!("cannot drop type {name} because other objects depend on it"),
            )
            .detail(dependents.join("\n"))
            .hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }
        for (rel, col) in columns {
            if let Some(r) = self.catalog.rel_mut(rel) {
                r.columns.retain(|c| c.id != col);
            }
        }
        let c = &mut self.catalog;
        c.domains.retain(|d| !uses(&d.base));
        for t in &mut c.composites {
            t.attributes.retain(|(_, ty)| !uses(ty));
        }
        c.enums.retain(|e| !(e.schema == schema && e.name == name));
        c.domains
            .retain(|d| !(d.schema == schema && d.name == name));
        c.composites
            .retain(|t| !(t.schema == schema && t.name == name));
        c.ranges.retain(|(s, n)| !(s == schema && n == name));
        Ok(())
    }

    fn drop_schema(&mut self, name: &str, cascade: bool) -> Result<()> {
        let mut contents: Vec<String> = Vec::new();
        for r in self.catalog.rels.iter().filter(|r| r.schema == name) {
            contents.push(format!(
                "{} {name}.{} depends on schema {name}",
                r.kind.word(),
                r.name
            ));
        }
        for t in self.catalog.enums.iter().filter(|t| t.schema == name) {
            contents.push(format!("type {name}.{} depends on schema {name}", t.name));
        }
        for t in self.catalog.domains.iter().filter(|t| t.schema == name) {
            contents.push(format!("type {name}.{} depends on schema {name}", t.name));
        }
        if !contents.is_empty() && !cascade {
            return Err(Fail::new(
                "2BP01",
                format!("cannot drop schema {name} because other objects depend on it"),
            )
            .detail(contents.join("\n"))
            .hint("Use DROP ... CASCADE to drop the dependent objects too."));
        }
        let ids: Vec<u32> = self
            .catalog
            .rels
            .iter()
            .filter(|r| r.schema == name)
            .map(|r| r.id)
            .collect();
        self.drop_rels(&ids, true, "table")?;
        let types: Vec<String> = self
            .catalog
            .enums
            .iter()
            .filter(|e| e.schema == name)
            .map(|e| e.name.clone())
            .chain(
                self.catalog
                    .domains
                    .iter()
                    .filter(|d| d.schema == name)
                    .map(|d| d.name.clone()),
            )
            .chain(
                self.catalog
                    .composites
                    .iter()
                    .filter(|d| d.schema == name)
                    .map(|d| d.name.clone()),
            )
            .collect();
        for t in types {
            self.drop_type(name, &t, true)?;
        }
        self.catalog.extensions.retain(|e| e.schema != name);
        self.catalog.sequences.retain(|(s, _)| s != name);
        self.catalog.schemas.retain(|s| s != name);
        Ok(())
    }

    // ---- Extensions and sequences ------------------------------------------------------

    pub fn create_extension(&mut self, f: &Value) -> Result<()> {
        let name = tree::str(f, "extname").to_owned();
        if name == "plpgsql" || self.catalog.extensions.iter().any(|e| e.name == name) {
            if tree::bool(f, "if_not_exists") {
                return Ok(());
            }
            return Err(Fail::new(
                "42710",
                format!("extension \"{name}\" already exists"),
            ));
        }
        let Some(info) = types::extension(&name) else {
            return Err(Fail::new(
                "0A000",
                format!("extension \"{name}\" is not available"),
            ));
        };
        let mut schema = None;
        let mut cascade = false;
        for o in tree::list(f, "options") {
            let Some(d) = tree::as_kind(o, "DefElem") else {
                continue;
            };
            match tree::str(d, "defname") {
                "schema" => {
                    schema = tree::field(d, "arg")
                        .and_then(tree::string)
                        .map(str::to_owned);
                }
                "cascade" => cascade = true,
                _ => {}
            }
        }
        let schema = self.creation_schema(schema.as_deref(), None)?;
        for required in info.requires {
            if !self.catalog.extensions.iter().any(|e| e.name == *required) {
                if !cascade {
                    return Err(Fail::new(
                        "42704",
                        format!("required extension \"{required}\" is not installed"),
                    )
                    .hint("Use CREATE EXTENSION ... CASCADE to install required extensions too."));
                }
                let version = types::extension(required).map_or("1.0", |e| e.version);
                self.catalog.extensions.push(Extension {
                    name: (*required).to_owned(),
                    schema: schema.clone(),
                    version: version.to_owned(),
                });
            }
        }
        if name == "postgis_topology" && !self.catalog.schemas.iter().any(|s| s == "topology") {
            self.catalog.schemas.push("topology".to_owned());
        }
        self.catalog.extensions.push(Extension {
            name,
            schema,
            version: info.version.to_owned(),
        });
        Ok(())
    }

    pub fn create_sequence(&mut self, f: &Value) -> Result<()> {
        let rv = tree::field(f, "sequence").unwrap_or(&Value::Null);
        let name = Name::from_range_var(rv);
        let schema = self.creation_schema(name.schema.as_deref(), tree::location(rv))?;
        if self.catalog.relation_name_taken(&schema, &name.name) {
            if tree::bool(f, "if_not_exists") {
                return Ok(());
            }
            return Err(Fail::new(
                "42P07",
                format!("relation \"{}\" already exists", name.name),
            )
            .at(tree::location(rv)));
        }
        self.catalog.sequences.push((schema, name.name));
        Ok(())
    }
}
