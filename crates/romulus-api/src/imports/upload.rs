//! An import's files, out of the request body: a multipart form (a folder
//! picked in the browser, files with their relative paths as names, or a
//! zip among them), a zip, or one SQL text (what an exporter printed).

use std::io::{Cursor, Read};

use axum::{
    body::Bytes,
    http::{HeaderMap, header},
};
use romulus_sandbox::import::SourceFile;

use crate::{ApiError, ApiResult, extract::read_body, orgs::invalid};

/// Request bodies above this are refused (413).
pub const BODY_LIMIT: usize = 32 << 20;
/// What a zip may unpack to, all files together.
const UNPACKED_LIMIT: u64 = 64 << 20;
/// One file, unpacked.
const FILE_LIMIT: u64 = 8 << 20;
/// Files in one import (a migrations folder with its down files and the
/// like fits many times over).
const MAX_FILES: usize = 5_000;

/// What the body is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Multipart,
    Zip,
    Text,
}

fn kind(headers: &HeaderMap, body: &[u8]) -> ApiResult<(Kind, Option<String>)> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let zipped = body.starts_with(b"PK\x03\x04");
    Ok(match mime.as_str() {
        "multipart/form-data" => (Kind::Multipart, multer::parse_boundary(content_type).ok()),
        "application/zip" | "application/x-zip-compressed" => (Kind::Zip, None),
        "text/plain" | "application/sql" | "text/x-sql" | "application/x-sql" => (Kind::Text, None),
        "" | "application/octet-stream" | "application/x-www-form-urlencoded" => {
            (if zipped { Kind::Zip } else { Kind::Text }, None)
        }
        other => {
            return Err(ApiError::UnsupportedMediaType(format!(
                "Send the migrations as multipart/form-data files, a zip (application/zip), or one SQL text (application/sql), not {other}."
            )));
        }
    })
}

/// Read the request's files. `filename` names a single SQL text
/// (`?filename=alembic.sql`).
pub async fn files(
    headers: &HeaderMap,
    body: axum::body::Body,
    filename: Option<&str>,
) -> ApiResult<Vec<SourceFile>> {
    let bytes = read_body(headers, body, BODY_LIMIT).await?;
    let (kind, boundary) = kind(headers, &bytes)?;
    let files = match kind {
        Kind::Multipart => {
            let boundary = boundary.ok_or_else(|| {
                ApiError::BadRequest("The multipart body has no boundary.".into())
            })?;
            multipart(bytes, boundary).await?
        }
        Kind::Zip => blocking(move || unzip(&bytes)).await??,
        Kind::Text => vec![text_file(
            filename.unwrap_or("migrations.sql"),
            bytes.to_vec(),
        )?],
    };
    if files.is_empty() {
        return Err(invalid(
            "files",
            "No file arrived: pick the folder that holds the migrations, or a zip of it.",
        ));
    }
    if files.len() > MAX_FILES {
        return Err(invalid(
            "files",
            &format!(
                "That is {} files; an import reads at most {MAX_FILES}.",
                files.len()
            ),
        ));
    }
    Ok(files)
}

async fn multipart(body: Bytes, boundary: String) -> ApiResult<Vec<SourceFile>> {
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut form = multer::Multipart::new(stream, boundary);
    let mut files = Vec::new();
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("The multipart body is malformed: {e}")))?
    {
        // Form fields without a file (none are expected) are ignored.
        let Some(name) = field.file_name().map(str::to_owned) else {
            continue;
        };
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::BadRequest(format!("The multipart body is malformed: {e}")))?;
        if data.starts_with(b"PK\x03\x04") || has_extension(&name, "zip") {
            // A zip among the files: its entries.
            let unpacked = blocking(move || unzip(&data)).await??;
            files.extend(unpacked);
        } else {
            files.push(text_file(&name, data.to_vec())?);
        }
        if files.len() > MAX_FILES {
            break;
        }
    }
    Ok(files)
}

fn has_extension(path: &str, extension: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// A file's text. SQL must be UTF-8; anything else (a README in another
/// encoding, a binary) is kept with whatever text it has, as the plan only
/// reads `.sql` files.
fn text_file(path: &str, bytes: Vec<u8>) -> ApiResult<SourceFile> {
    let path = path.replace('\\', "/");
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > FILE_LIMIT {
        return Err(invalid(
            "files",
            &format!(
                "{path} is {} MiB; one migration file can be at most {} MiB.",
                bytes.len() >> 20,
                FILE_LIMIT >> 20
            ),
        ));
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) if !has_extension(&path, "sql") => {
            String::from_utf8_lossy(err.as_bytes()).into_owned()
        }
        Err(_) => {
            return Err(ApiError::Unprocessable {
                code: "invalid_file",
                message: format!("{path} is not UTF-8 text: save it as UTF-8 and import again."),
                details: Some(serde_json::json!({ "path": path })),
            });
        }
    };
    Ok(SourceFile::new(path, text))
}

/// A zip's files. Paths that would leave the archive (`../x`, absolute)
/// are refused, sizes are checked as they are read (not as the archive
/// claims them).
fn unzip(bytes: &[u8]) -> ApiResult<Vec<SourceFile>> {
    let unreadable = |e: zip::result::ZipError| ApiError::Unprocessable {
        code: "invalid_file",
        message: format!("The zip could not be read: {e}."),
        details: None,
    };
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(unreadable)?;
    if archive.len() > MAX_FILES * 2 {
        return Err(invalid(
            "files",
            &format!(
                "The zip holds {} entries; an import reads at most {MAX_FILES} files.",
                archive.len()
            ),
        ));
    }
    let mut files = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(unreadable)?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            return Err(ApiError::Unprocessable {
                code: "invalid_file",
                message: format!(
                    "The zip entry {:?} points outside the archive.",
                    entry.name()
                ),
                details: None,
            });
        };
        let path = name.to_string_lossy().into_owned();
        let mut data = Vec::new();
        (&mut entry)
            .take(FILE_LIMIT + 1)
            .read_to_end(&mut data)
            .map_err(|e| ApiError::Unprocessable {
                code: "invalid_file",
                message: format!("{path} could not be unpacked: {e}."),
                details: None,
            })?;
        total += u64::try_from(data.len()).unwrap_or(u64::MAX);
        if total > UNPACKED_LIMIT {
            return Err(invalid(
                "files",
                &format!("The zip unpacks to more than {} MiB.", UNPACKED_LIMIT >> 20),
            ));
        }
        files.push(text_file(&path, data)?);
    }
    Ok(files)
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use axum::http::HeaderValue;

    use super::*;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn zips_unpack_with_their_paths() {
        let bytes = zip_of(&[
            ("migrations/0001_init.sql", b"CREATE TABLE a ();"),
            ("migrations/README.md", b"# notes"),
        ]);
        let files = unzip(&bytes).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["migrations/0001_init.sql", "migrations/README.md"]);
        assert_eq!(files[0].text, "CREATE TABLE a ();");
    }

    #[test]
    fn zip_entries_leaving_the_archive_are_refused() {
        let bytes = zip_of(&[("../../etc/x.sql", b"x")]);
        assert!(unzip(&bytes).is_err());
    }

    #[test]
    fn sql_must_be_utf8_other_files_need_not() {
        assert!(text_file("a.sql", vec![0xff, 0xfe]).is_err());
        assert!(text_file("logo.png", vec![0xff, 0xfe]).is_ok());
    }

    #[test]
    fn the_content_type_or_the_bytes_say_what_the_body_is() {
        let typed = |t: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(t));
            headers
        };
        let kind_of = |t, body: &[u8]| kind(&typed(t), body).map(|(k, _)| k).ok();
        assert_eq!(kind_of("application/zip", b"x"), Some(Kind::Zip));
        assert_eq!(kind_of("application/sql", b"x"), Some(Kind::Text));
        assert_eq!(
            kind_of("application/octet-stream", b"PK\x03\x04"),
            Some(Kind::Zip)
        );
        assert_eq!(
            kind_of("application/x-www-form-urlencoded", b"-- x"),
            Some(Kind::Text)
        );
        assert_eq!(
            kind_of("multipart/form-data; boundary=abc", b""),
            Some(Kind::Multipart)
        );
        assert_eq!(kind_of("image/png", b""), None);
    }
}
