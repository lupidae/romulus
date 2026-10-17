//! The SQL a schema derived from a migration is kept as: by a version
//! published from a migration-mode draft, and by a draft going back to
//! schema mode.
//!
//! Canonical SQL (`tidy::canonical_sql`) when it compiles back to the same
//! model, as Tidy checks; otherwise the base's SQL then the migration as
//! they compiled (`romulus_sandbox::draft_migration::faithful_sql`), checked
//! the same way. When neither compiles back (a function stub guessed
//! differently for the two texts, say), nothing is stored: 422
//! `unrepresentable`, naming what would come out differently. Never a
//! source that says less, or other, than its model: canonical SQL has gaps
//! (foreign tables, `NOT VALID` checks…), and a lossy source would lose
//! those objects at the next draft, then at its publish.
//!
//! Each check is a sandbox compile, counted against the person's and the
//! database's compile limits like any other ([`Checks`]).

use romulus_diff::SchemaDiff;
use serde_json::json;
use uuid::Uuid;

use super::tidy::canonical_sql;
use crate::{
    ApiError, ApiResult,
    collab::Hub,
    domain::{PgMajor, Schema},
    rate::Limits,
};

/// Who the checking compiles are for, and where they run.
pub(super) struct Checks<'a> {
    pub hub: &'a Hub,
    pub limits: &'a Limits,
    /// The person asking (`Limits::compile`).
    pub user: Uuid,
    /// The draft's database (`Limits::database_compiles`).
    pub database: Uuid,
    /// What the draft compiles on (its database's major).
    pub major: PgMajor,
}

/// What the refusal is about: what did not happen.
#[derive(Debug, Clone, Copy)]
pub(super) enum Refused {
    Publish,
    Discard,
}

impl Checks<'_> {
    /// What of `model` comes out differently from `sql`: `None` when it
    /// compiles to exactly `model` (their `database` names aside), else the
    /// objects that differ (none named when it does not compile at all).
    async fn differences(&self, sql: &str, model: &Schema) -> ApiResult<Option<Vec<String>>> {
        self.limits.compile.check(self.user)?;
        self.limits.database_compiles.check(self.database)?;
        let Some(mut compiled) = self.hub.compiler().compile(self.major, sql).await?.model else {
            return Ok(Some(Vec::new()));
        };
        compiled.database.clone_from(&model.database);
        if compiled == *model {
            return Ok(None);
        }
        let model = model.clone();
        let objects =
            tokio::task::spawn_blocking(move || objects(&romulus_diff::diff(&compiled, &model)))
                .await
                .map_err(ApiError::internal)?;
        Ok(Some(objects))
    }

    /// Canonical SQL of `model`, if it compiles back to it.
    async fn canonical(&self, model: &Schema) -> ApiResult<Option<String>> {
        let rendered = {
            let model = model.clone();
            tokio::task::spawn_blocking(move || canonical_sql(&model))
                .await
                .map_err(ApiError::internal)?
        };
        Ok(self
            .differences(&rendered, model)
            .await?
            .is_none()
            .then_some(rendered))
    }

    /// The base's SQL then the migration, if that compiles back to `model`;
    /// 422 `unrepresentable` otherwise.
    async fn faithful(
        &self,
        model: &Schema,
        base_sql: &str,
        migration: &str,
        refused: Refused,
    ) -> ApiResult<String> {
        let sql = romulus_sandbox::draft_migration::faithful_sql(base_sql, migration);
        match self.differences(&sql, model).await? {
            None => Ok(sql),
            Some(objects) => {
                tracing::warn!(?objects, "no SQL compiles back to the schema; refused");
                Err(unrepresentable(&objects, refused))
            }
        }
    }

    /// SQL for `model`, which `base_sql` then `migration` compiled to: see
    /// the module documentation.
    pub(super) async fn schema_source(
        &self,
        model: &Schema,
        base_sql: &str,
        migration: &str,
    ) -> ApiResult<String> {
        if let Some(sql) = self.canonical(model).await? {
            return Ok(sql);
        }
        tracing::info!("canonical SQL loses part of the schema: kept as base + migration");
        self.faithful(model, base_sql, migration, Refused::Publish)
            .await
    }

    /// A draft going back to schema mode: its SQL, and what
    /// `drafts.base_sql` becomes (`None`: the base version's own text).
    /// Canonical SQL of the schema and of the base when both compile back
    /// (an update then merges like with like); otherwise the base's SQL then
    /// the migration, relative to the base's own text, if that compiles
    /// back; 422 `unrepresentable` otherwise.
    pub(super) async fn back_to_schema(
        &self,
        model: &Schema,
        base: Option<&Schema>,
        base_sql: &str,
        migration: &str,
    ) -> ApiResult<(String, Option<String>)> {
        if let Some(sql) = self.canonical(model).await? {
            match base {
                None => return Ok((sql, Some(String::new()))),
                Some(base) => {
                    if let Some(base_canonical) = self.canonical(base).await? {
                        return Ok((sql, Some(base_canonical)));
                    }
                }
            }
        }
        tracing::info!("canonical SQL loses part of the schema: back to base + migration");
        let sql = self
            .faithful(model, base_sql, migration, Refused::Discard)
            .await?;
        Ok((sql, None))
    }
}

/// The objects a diff is about, qualified (`public.remote`, an extension's
/// bare name), in its order.
fn objects(diff: &SchemaDiff) -> Vec<String> {
    let qualified = |schema: &str, name: &str| format!("{schema}.{name}");
    diff.entities
        .iter()
        .map(|e| qualified(&e.schema, &e.name))
        .chain(diff.enums.iter().map(|e| qualified(&e.schema, &e.name)))
        .chain(diff.domains.iter().map(|d| qualified(&d.schema, &d.name)))
        .chain(
            diff.composites
                .iter()
                .map(|c| qualified(&c.schema, &c.name)),
        )
        .chain(diff.extensions.iter().map(|e| e.name.clone()))
        .collect()
}

/// 422 `unrepresentable`: no SQL romulus can keep compiles back to the
/// schema; `details.objects` are what would come out differently.
fn unrepresentable(objects: &[String], refused: Refused) -> ApiError {
    let what = match objects {
        [] => "this schema".to_owned(),
        few if few.len() <= 3 => few.join(", "),
        [first, second, third, rest @ ..] => {
            format!("{first}, {second}, {third} and {} more", rest.len())
        }
        _ => unreachable!("more than three"),
    };
    let outcome = match refused {
        Refused::Publish => "Nothing was published.",
        Refused::Discard => "The migration was kept.",
    };
    ApiError::Unprocessable {
        code: "unrepresentable",
        message: format!(
            "No SQL romulus can keep compiles back to what the migration made: {what} would come out differently. {outcome}"
        ),
        details: Some(json!({ "objects": objects })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_a_few_objects_then_counts() {
        let names = |n: usize| (0..n).map(|k| format!("public.t{k}")).collect::<Vec<_>>();
        let message = |n| match unrepresentable(&names(n), Refused::Publish) {
            ApiError::Unprocessable { message, .. } => message,
            other => panic!("{other:?}"),
        };
        assert!(message(0).contains(": this schema would"));
        assert!(message(1).contains(": public.t0 would"));
        assert!(message(3).contains(": public.t0, public.t1, public.t2 would"));
        assert!(message(5).contains(": public.t0, public.t1, public.t2 and 2 more would"));
        assert!(message(1).ends_with("Nothing was published."));
    }
}
