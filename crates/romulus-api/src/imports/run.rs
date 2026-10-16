//! Running an import: the replay in the sandbox, its progress, the
//! versions stored, the report.
//!
//! The job runs in a task of its own: a client that goes away (a closed
//! tab, a CI runner cancelled) does not leave it half done, it finishes and
//! stores (importing again then creates nothing). Its progress goes to the
//! client as NDJSON when asked for (`Accept: application/x-ndjson`): the
//! headers at once, an event per line, and a blank line after
//! [`KEEP_ALIVE`] without one, so that no proxy gives up on a long import
//! (the plain JSON answer comes only at the end: Cloudflare answers 524
//! after about 100 s without a byte, though the import still lands).

use std::{convert::Infallible, time::Instant};

use axum::{
    body::{Body, Bytes},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use remus_core::Schema;
use romulus_sandbox::{ReplayEvent, import::Migration};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    IgnoredFile, ImportOptions, SkippedStatement,
    plan::{self, ImportPreview, MigrationStatus, Planned},
    staging,
    turns::{Place, Running},
};
use crate::{
    ApiError, ApiResult, AppState,
    domain::MigrationFormat,
    error::ErrorBody,
    extract::Json,
    versions::{
        layout::VersionLayout,
        store::{ImportedVersion, NewImport, store_import},
    },
};

/// Who imports.
#[derive(Debug, Clone, Copy)]
pub enum Author {
    User(Uuid),
    Token(Uuid),
}

/// How an import ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    /// Versions were created (201).
    Created,
    /// Every migration was imported already: nothing to do (200).
    Unchanged,
    /// A dry run: everything replayed, nothing stored (200).
    Checked,
}

/// One migration in the report.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportedMigration {
    pub id: String,
    pub name: String,
    pub path: String,
    /// `new` (created by this import, or would be in a dry run) or
    /// `imported` (already, untouched).
    pub status: MigrationStatus,
    /// Its version number (`null` in a dry run).
    pub version: Option<i32>,
    /// What it changed, against the version before it: `"2 tables added"`.
    pub changes: Option<String>,
    /// What was not replayed.
    pub skipped: Vec<SkippedStatement>,
}

/// What an import did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportReport {
    pub status: ImportStatus,
    pub format: MigrationFormat,
    /// Every migration of the files, in order.
    pub migrations: Vec<ImportedMigration>,
    pub ignored: Vec<IgnoredFile>,
    /// Versions created.
    pub created: usize,
    pub duration_ms: u64,
}

/// One line of an import's progress (`Accept: application/x-ndjson`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ImportEvent {
    /// The files, planned: what will be replayed.
    Planned { preview: ImportPreview },
    /// Other imports are replaying: this one waits for its turn
    /// (organizations take turns).
    Queued,
    /// Every migration ran once in the sandbox: they replay; versions come
    /// next, one per migration.
    Checked,
    /// A migration's model was read.
    Replayed {
        /// Its position in `preview.migrations`.
        index: usize,
        /// The version number it will get.
        number: i32,
        /// What it changes: `"2 tables added"`.
        changes: String,
    },
    /// The import finished (the last line).
    Done { report: ImportReport },
    /// The import stopped, nothing stored (the last line): 422
    /// `import_failed` (`details`: `ImportFailure`), 409, 503…
    Failed { error: ErrorBody },
}

/// The database imported into.
pub struct Target {
    /// Whose turn the replay waits for.
    pub org_id: Uuid,
    pub database_id: Uuid,
    /// The models' `database`.
    pub slug: String,
}

/// Silence after which the NDJSON stream sends a blank line.
pub const KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// Plan, refuse what cannot be imported, then run (see the module
/// documentation).
pub async fn start(
    state: AppState,
    target: Target,
    author: Author,
    files: Vec<romulus_sandbox::import::SourceFile>,
    options: ImportOptions,
    stream: bool,
) -> ApiResult<Response> {
    let started = Instant::now();
    let mut planned = plan::plan(&state.db, target.database_id, files, &options).await?;
    // One import of a database at a time, settled before any answer goes
    // out: a retry of this one (a proxy stopped waiting for the answer)
    // gets a real 409, streamed or not. Once it is this import's turn, the
    // history may have moved since it was planned: match again.
    let running = if planned.new.is_empty() {
        None
    } else {
        let running = state.imports.running(
            target.database_id,
            plan::fingerprint(&planned, options.dry_run),
        )?;
        planned = plan::rematch(&state.db, target.database_id, planned).await?;
        Some(running)
    };
    if let Some(refusal) = plan::refusal(&planned, &options) {
        return Err(refusal);
    }
    let job = Job {
        state,
        target,
        author,
        options,
        planned,
        started,
        running,
    };
    if stream {
        let (events, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let _ = job.run(Some(events)).await;
        });
        let body = ndjson(received, KEEP_ALIVE);
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/x-ndjson"),
                (header::CACHE_CONTROL, "no-cache"),
                // Proxies must pass lines on as they come.
                (header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            Body::from_stream(body),
        )
            .into_response());
    }
    let report = tokio::spawn(job.run(None))
        .await
        .map_err(ApiError::internal)??;
    let status = if report.status == ImportStatus::Created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(report)).into_response())
}

/// The events as NDJSON lines, a blank line after `keep_alive` without
/// one; ends when the job drops its sender.
fn ndjson(
    received: mpsc::UnboundedReceiver<ImportEvent>,
    keep_alive: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> {
    futures_util::stream::unfold(received, move |mut received| async move {
        let line = match tokio::time::timeout(keep_alive, received.recv()).await {
            Ok(Some(event)) => {
                let mut line = serde_json::to_vec(&event).unwrap_or_default();
                line.push(b'\n');
                line
            }
            Ok(None) => return None,
            // Nothing to say yet: a blank line keeps proxies waiting.
            Err(_) => b"\n".to_vec(),
        };
        Some((Ok(Bytes::from(line)), received))
    })
}

struct Job {
    state: AppState,
    target: Target,
    author: Author,
    options: ImportOptions,
    planned: Planned,
    started: Instant,
    /// The database's import guard (for as long as the job runs), when it
    /// has migrations to replay.
    running: Option<Running>,
}

type Events = mpsc::UnboundedSender<ImportEvent>;

impl Job {
    /// The job, start to end. Its outcome also ends the stream.
    async fn run(self, events: Option<Events>) -> ApiResult<ImportReport> {
        if let Some(events) = &events {
            let _ = events.send(ImportEvent::Planned {
                preview: self.planned.preview.clone(),
            });
        }
        let outcome = self.import(events.as_ref()).await;
        if let Some(events) = &events {
            let _ = events.send(match &outcome {
                Ok(report) => ImportEvent::Done {
                    report: report.clone(),
                },
                Err(err) => ImportEvent::Failed { error: err.body() },
            });
        }
        if let Err(err) = &outcome {
            tracing::info!(database = %self.target.database_id, code = err.code(), "import refused");
        }
        outcome
    }

    async fn import(&self, events: Option<&Events>) -> ApiResult<ImportReport> {
        let planned = &self.planned;
        let Some(running) = &self.running else {
            // Nothing new (maybe stored meanwhile by the same import, sent
            // twice): nothing left to import.
            return Ok(self.report(ImportStatus::Unchanged, &[], &[], &[]));
        };
        let new: Vec<&Migration> = planned
            .new
            .iter()
            .map(|&k| &planned.migrations[k])
            .collect();
        let mut texts: Vec<String> = planned.history.iter().map(|e| e.source.clone()).collect();
        let from = texts.len();
        texts.extend(new.iter().map(|m| m.sql.clone()));

        // Models go to the staging table as they are read (a dry run keeps
        // none): the job holds one at a time.
        let staging = (!self.options.dry_run).then(|| running.staging());
        let replayed = self.replay(&new, texts, from, events, staging).await;
        let Replayed { changes, skipped } = match replayed {
            Ok(replayed) => replayed,
            Err(err) => {
                if let Some(staging) = staging {
                    staging::discard(&self.state.db, staging).await;
                }
                return Err(err);
            }
        };
        let Some(staging) = staging else {
            return Ok(self.report(ImportStatus::Checked, &[], &changes, &skipped));
        };

        let (created_by, created_by_token) = match self.author {
            Author::User(id) => (Some(id), None),
            Author::Token(id) => (None, Some(id)),
        };
        let versions = new
            .iter()
            .zip(&skipped)
            .map(|(migration, skipped)| ImportedVersion {
                source: migration.sql.clone(),
                message: migration.name.clone(),
                migration_id: migration.id.clone(),
                path: migration.path.clone(),
                checksum: plan::checksum(&migration.sql),
                // Lines in the migration's own text (the version's source),
                // which is what the version shows next to them.
                skipped: serde_json::to_value(
                    skipped
                        .iter()
                        .map(|s| SkippedStatement {
                            line: s.line - migration.line.saturating_sub(1),
                            ..s.clone()
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default(),
            })
            .collect();
        let stored = store_import(
            &self.state.db,
            NewImport {
                database_id: self.target.database_id,
                format: planned.format,
                created_by,
                created_by_token,
                append: self.options.append,
                imported_before: planned.history.len(),
                latest: planned.latest.as_ref().map(|l| l.number),
                staging,
                versions,
            },
        )
        .await;
        let stored = match stored {
            Ok(stored) => stored,
            Err(err) => {
                staging::discard(&self.state.db, staging).await;
                return Err(err.into());
            }
        };
        if let Some((_, last)) = stored.last() {
            // Open drafts of the database learn their base moved.
            self.state
                .collab
                .version_created(self.target.database_id, *last);
        }
        let numbers: Vec<i32> = stored.iter().map(|(_, n)| *n).collect();
        Ok(self.report(ImportStatus::Created, &numbers, &changes, &skipped))
    }

    /// The replay of the history and the new migrations: what each new one
    /// changes and skips (its model staged under `staging`), or the error
    /// naming the failing one.
    async fn replay(
        &self,
        new: &[&Migration],
        texts: Vec<String>,
        from: usize,
        events: Option<&Events>,
        staging: Option<Uuid>,
    ) -> ApiResult<Replayed> {
        let planned = &self.planned;
        // One import replays at a time; organizations take turns.
        let _turn = match self.state.imports.take(self.target.org_id)? {
            Place::Now(turn) => turn,
            Place::Waiting(wait) => {
                if let Some(events) = events {
                    let _ = events.send(ImportEvent::Queued);
                }
                wait.turn().await?
            }
        };
        // A model at a time: the replay waits while the last one is
        // compared and staged, and stops if staging stops.
        let (sender, received) = mpsc::channel(1);
        let staged = tokio::spawn(self.stage(received, events.cloned(), from, staging));
        let replay = self.state.compiler.replay(texts, from, sender).await;
        let staged = staged.await.map_err(ApiError::internal)?;
        // Why staging stopped comes first: the replay only stopped with it.
        let changes = staged?;
        let replay = replay?;

        let skipped: Vec<Vec<SkippedStatement>> = new
            .iter()
            .enumerate()
            .map(|(k, m)| {
                replay.skipped[from + k]
                    .iter()
                    .map(|s| plan::skipped_in_file(s, m))
                    .collect()
            })
            .collect();
        if let Some(failure) = &replay.failure {
            // A failure in a migration imported before cannot happen (it
            // replayed then); name the first new one if it does.
            let k = failure.index.saturating_sub(from).min(new.len() - 1);
            let migration = new[k];
            let errors = failure
                .diagnostics
                .iter()
                .filter(|d| d.is_error())
                .map(|d| plan::located(d, migration))
                .collect();
            let index = planned.new[k];
            return Err(plan::failed(
                index,
                &planned.preview.migrations[index],
                errors,
            ));
        }
        if changes.len() != new.len() {
            return Err(ApiError::Internal(
                format!(
                    "the replay read {} models for {} migrations",
                    changes.len(),
                    new.len()
                )
                .into(),
            ));
        }
        Ok(Replayed { changes, skipped })
    }

    /// Takes the replay's events: passes progress on to the client, and
    /// for each model, what it changes against the one before it (the
    /// latest version for the first) and its layout (the previous one,
    /// pruned to it), then stages it. One model in hand at a time; the
    /// models' total size is held to the budget. Ends with the replay (its
    /// sender dropped), or stops it by returning.
    fn stage(
        &self,
        mut received: mpsc::Receiver<ReplayEvent>,
        sender: Option<Events>,
        from: usize,
        staging: Option<Uuid>,
    ) -> impl Future<Output = ApiResult<Vec<String>>> + Send + 'static {
        let planned = &self.planned;
        let first_number = planned.preview.first_number.unwrap_or(1);
        let latest = planned.latest.as_ref();
        let mut previous = latest.and_then(|l| Schema::deserialize(&l.model).ok());
        let mut layout = latest
            .map(|l| VersionLayout::from_value(&l.layout))
            .unwrap_or_default();
        let indexes = planned.new.clone();
        let slug = self.target.slug.clone();
        let db = self.state.db.clone();
        let database_id = self.target.database_id;
        let budget = self.state.imports.model_budget();
        async move {
            let mut changes = Vec::with_capacity(indexes.len());
            let mut bytes = 0usize;
            while let Some(event) = received.recv().await {
                let (index, model) = match event {
                    ReplayEvent::Checked => {
                        if let Some(sender) = &sender {
                            let _ = sender.send(ImportEvent::Checked);
                        }
                        continue;
                    }
                    ReplayEvent::Replayed { index, model } => (index, model),
                };
                let k = index - from;
                let slug = slug.clone();
                let before = previous.take();
                let layout_before = std::mem::take(&mut layout);
                // Diffing and serialising megabytes: off the async workers.
                let compared = tokio::task::spawn_blocking(move || {
                    let mut model = *model;
                    slug.clone_into(&mut model.database);
                    let summary = changes_between(before.as_ref(), &model);
                    let layout = layout_before.pruned(&model);
                    let json = serde_json::to_string(&model)?;
                    let layout_json = serde_json::to_string(&layout)?;
                    Ok::<_, serde_json::Error>((model, summary, layout, json, layout_json))
                })
                .await
                .map_err(ApiError::internal)?
                .map_err(ApiError::internal)?;
                let (model, summary, next_layout, json, layout_json) = compared;
                bytes += json.len();
                if bytes > budget {
                    return Err(too_large(budget));
                }
                if let Some(staging) = staging {
                    sqlx::query!(
                        "INSERT INTO import_staging (import_id, ordinal, database_id, model, layout)
                         VALUES ($1, $2, $3, $4::text::jsonb, $5::text::jsonb)",
                        staging,
                        i32::try_from(k).map_err(ApiError::internal)?,
                        database_id,
                        json,
                        layout_json,
                    )
                    .execute(&db)
                    .await?;
                }
                if let Some(sender) = &sender {
                    let _ = sender.send(ImportEvent::Replayed {
                        index: indexes[k],
                        number: first_number + i32::try_from(k).unwrap_or(i32::MAX),
                        changes: summary.clone(),
                    });
                }
                previous = Some(model);
                layout = next_layout;
                changes.push(summary);
            }
            Ok(changes)
        }
    }

    /// The report: every migration of the files, the new ones with what
    /// this import made of them.
    fn report(
        &self,
        status: ImportStatus,
        numbers: &[i32],
        changes: &[String],
        skipped: &[Vec<SkippedStatement>],
    ) -> ImportReport {
        let preview = &self.planned.preview;
        let migrations = preview
            .migrations
            .iter()
            .enumerate()
            .map(|(index, m)| {
                let k = self.planned.new.iter().position(|&n| n == index);
                ImportedMigration {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    path: m.path.clone(),
                    status: m.status,
                    version: match k {
                        Some(k) => numbers.get(k).copied(),
                        None => m.version,
                    },
                    changes: k.and_then(|k| changes.get(k).cloned()),
                    skipped: k
                        .and_then(|k| skipped.get(k).cloned())
                        .unwrap_or_else(|| m.skipped.clone()),
                }
            })
            .collect();
        ImportReport {
            status,
            format: preview.format,
            migrations,
            ignored: preview.ignored.clone(),
            created: numbers.len(),
            duration_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// What a replay made of the new migrations (their models are staged).
struct Replayed {
    changes: Vec<String>,
    skipped: Vec<Vec<SkippedStatement>>,
}

/// 422 `models_too_large`: the versions would hold more than `budget`
/// bytes of models.
fn too_large(budget: usize) -> ApiError {
    ApiError::Unprocessable {
        code: "models_too_large",
        message: format!(
            "These migrations' schemas add up to more than {} MiB of versions (each version keeps its whole schema). Import them in several parts, the oldest first. Nothing was imported.",
            budget >> 20
        ),
        details: None,
    }
}

/// `"2 tables added, 1 changed"`, against the model before (nothing for
/// the first version).
fn changes_between(previous: Option<&Schema>, model: &Schema) -> String {
    if let Some(previous) = previous {
        return romulus_diff::diff(previous, model).summary.text;
    }
    romulus_diff::diff(&crate::versions::empty(model), model)
        .summary
        .text
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;

    use super::*;

    #[tokio::test]
    async fn a_silent_stream_sends_blank_lines_until_an_event_comes() {
        let (sender, received) = mpsc::unbounded_channel();
        let mut stream = Box::pin(ndjson(received, Duration::from_millis(10)));
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(&first[..], b"\n");
        sender.send(ImportEvent::Queued).unwrap();
        let mut line = stream.next().await.unwrap().unwrap();
        while &line[..] == b"\n" {
            line = stream.next().await.unwrap().unwrap();
        }
        assert_eq!(&line[..], b"{\"event\":\"queued\"}\n");
        drop(sender);
        while let Some(chunk) = stream.next().await {
            assert_eq!(&chunk.unwrap()[..], b"\n");
        }
    }
}
