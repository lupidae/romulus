//! Resources: each database's versions as SQL, `romulus://<org>/<db>/v<n>`
//! (the list names each database's latest; the template reaches any), the
//! CI guide, `romulus://guide/ci`, and the exporters an agent runs to import
//! code migrations (`romulus://export-migrations/<tool>.sh`, the scripts of
//! `scripts/export-migrations`).

use serde_json::{Value, json};

use super::tools::{Ctx, ToolError};
use crate::{databases::Database, orgs::Org, versions::VersionTexts};

const CI_GUIDE: &str = "romulus://guide/ci";
/// The exporters of migrations written in code: what they print is what
/// `import_migrations` takes (format `alembic`, `django`, `rails`).
const EXPORTERS: [(&str, &str, &str); 3] = [
    (
        "romulus://export-migrations/alembic.sh",
        "Alembic: prints every migration's SQL (offline mode, no database). Run next to alembic.ini.",
        include_str!("../../../../scripts/export-migrations/alembic.sh"),
    ),
    (
        "romulus://export-migrations/django.sh",
        "Django: prints every migration's SQL, applying each to an empty scratch database (--database ALIAS). Run next to manage.py.",
        include_str!("../../../../scripts/export-migrations/django.sh"),
    ),
    (
        "romulus://export-migrations/rails.sh",
        "Rails: prints the SQL each migration runs on an empty scratch database (DATABASE_URL). Run from the app's root.",
        include_str!("../../../../scripts/export-migrations/rails.sh"),
    ),
];
/// Databases the list names at most.
const LISTED: usize = 100;

pub fn templates() -> Value {
    json!({
        "resourceTemplates": [{
            "uriTemplate": "romulus://{org}/{database}/v{version}",
            "name": "schema_version",
            "title": "A version of a database's schema",
            "description": "The SQL of version `version` of a database (org and database slugs).",
            "mimeType": "application/sql",
        }],
    })
}

pub async fn list(ctx: &Ctx) -> Result<Value, ToolError> {
    let mut resources = vec![json!({
        "uri": CI_GUIDE,
        "name": "ci_guide",
        "title": "Pushing from CI with remus",
        "mimeType": "text/plain",
    })];
    for (uri, description, _) in EXPORTERS {
        let file = uri.rsplit('/').next().unwrap_or(uri);
        resources.push(json!({
            "uri": uri,
            "name": file,
            "title": format!("Migration exporter ({file})"),
            "description": description,
            "mimeType": "text/x-shellscript",
        }));
    }
    let orgs: Vec<Org> = ctx.api.get(&["orgs"], &[]).await?;
    'orgs: for org in &orgs {
        let databases: Vec<Database> = ctx.api.get(&["orgs", &org.slug, "databases"], &[]).await?;
        for database in databases {
            if resources.len() > LISTED {
                break 'orgs;
            }
            let Some(latest) = database.latest else {
                continue;
            };
            resources.push(json!({
                "uri": format!("romulus://{}/{}/v{}", org.slug, database.slug, latest.number),
                "name": format!("{}/{} v{}", org.slug, database.slug, latest.number),
                "title": format!("{} ({}), latest version", database.name, org.name),
                "mimeType": "application/sql",
            }));
        }
    }
    Ok(json!({ "resources": resources }))
}

/// What `uri` names, or `None` when it names nothing (the API's own refusal
/// otherwise: not a member, no such version).
pub async fn read(ctx: &Ctx, uri: &str) -> Result<Option<Value>, ToolError> {
    if uri == CI_GUIDE {
        let command = format!(
            "remus -f json | curl -fsS -H \"Authorization: Bearer $ROMULUS_TOKEN\" \\\n  \
             --data-binary @- {}\n",
            ctx.public_url
                .join("/api/databases/<database id>/versions")
                .map_or_else(|_| String::new(), String::from)
        );
        return Ok(Some(contents(
            uri,
            "text/plain",
            &super::tools::ci_guide(&command, None),
        )));
    }
    if let Some((_, _, script)) = EXPORTERS.iter().find(|(known, _, _)| *known == uri) {
        return Ok(Some(contents(uri, "text/x-shellscript", script)));
    }
    let Some((org, database, number)) = parse(uri) else {
        return Ok(None);
    };
    let n = number.to_string();
    let texts: VersionTexts = ctx
        .api
        .get(
            &["orgs", org, "databases", database, "versions", &n, "texts"],
            &[],
        )
        .await?;
    Ok(Some(contents(uri, "application/sql", &texts.sql)))
}

fn contents(uri: &str, mime: &str, text: &str) -> Value {
    json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] })
}

/// `romulus://acme/billing/v12` → (`acme`, `billing`, 12).
fn parse(uri: &str) -> Option<(&str, &str, i32)> {
    let rest = uri.strip_prefix("romulus://")?;
    let mut parts = rest.split('/');
    let (org, database, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || org.is_empty() || database.is_empty() {
        return None;
    }
    let number = version.strip_prefix('v')?.parse().ok().filter(|n| *n > 0)?;
    Some((org, database, number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_uris_name_an_org_a_database_and_a_number() {
        assert_eq!(
            parse("romulus://acme/billing/v12"),
            Some(("acme", "billing", 12))
        );
        for bad in [
            "romulus://acme/billing/12",
            "romulus://acme/billing/v0",
            "romulus://acme/v1",
            "romulus://acme/billing/v1/x",
            "file:///etc/passwd",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
