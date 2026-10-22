//! The migrations folder: every text file in it, by its path relative to
//! the folder (with `/`), sent as romulus's import reads them. romulus
//! decides what is a migration (the tool's naming, down files, its own
//! files); here only hidden entries (`.git`, `.DS_Store`) and files that
//! are not UTF-8 text are left out. Files go in batches that fit a message,
//! a large one in parts.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// A file, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Relative to the folder, with `/`.
    pub path: String,
    pub content: String,
}

/// Files an import reads, at most.
pub const MAX_FILES: usize = 5_000;
/// One file, at most (romulus refuses larger ones).
pub const MAX_FILE_BYTES: usize = 8 << 20;
/// Content per message: a message carries 4 MiB at most, JSON-escaped.
pub const BATCH_BYTES: usize = 2 << 20;

#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    #[error("{path}: {error}")]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("{0} is not a folder: give the migrations folder")]
    NotAFolder(PathBuf),
    #[error("{0} holds no file")]
    Empty(PathBuf),
    #[error("{0} holds more than {MAX_FILES} files: give the migrations folder itself")]
    TooMany(PathBuf),
    #[error("{0} is larger than 8 MiB: romulus imports smaller migration files")]
    TooLarge(String),
}

/// The folder's files, sorted by path, and the ones skipped (not text).
pub fn read(dir: &Path) -> Result<(Vec<File>, Vec<String>), FilesError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |error| FilesError::Io { path, error }
    };
    if !fs::metadata(dir).map_err(io(dir))?.is_dir() {
        return Err(FilesError::NotAFolder(dir.to_owned()));
    }
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut pending = vec![(dir.to_owned(), String::new())];
    while let Some((folder, prefix)) = pending.pop() {
        for entry in fs::read_dir(&folder).map_err(io(&folder))? {
            let entry = entry.map_err(io(&folder))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let relative = format!("{prefix}{name}");
            // Follows links, as a checkout's tools would.
            let meta = fs::metadata(&path).map_err(io(&path))?;
            if meta.is_dir() {
                pending.push((path, format!("{relative}/")));
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if usize::try_from(meta.len()).unwrap_or(usize::MAX) > MAX_FILE_BYTES {
                return Err(FilesError::TooLarge(relative));
            }
            match String::from_utf8(fs::read(&path).map_err(io(&path))?) {
                Ok(content) => files.push(File {
                    path: relative,
                    content,
                }),
                Err(_) => skipped.push(relative),
            }
            if files.len() > MAX_FILES {
                return Err(FilesError::TooMany(dir.to_owned()));
            }
        }
    }
    if files.is_empty() {
        return Err(FilesError::Empty(dir.to_owned()));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort();
    Ok((files, skipped))
}

/// A file or a part of one, as an upload takes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Piece<'a> {
    pub path: &'a str,
    pub content: &'a str,
    /// From 1, for a file sent in parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<u32>,
}

/// The files in batches of about `limit` bytes of content each; a file
/// larger than `limit` in parts of its own (cut on character boundaries).
pub fn batches(files: &[File], limit: usize) -> Vec<Vec<Piece<'_>>> {
    let mut out: Vec<Vec<Piece<'_>>> = Vec::new();
    let mut current: Vec<Piece<'_>> = Vec::new();
    let mut size = 0;
    for file in files {
        // A path and its JSON escaping count too.
        let weight = file.content.len() + file.path.len() + 64;
        if file.content.len() > limit {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                size = 0;
            }
            for (k, part) in parts(&file.content, limit).into_iter().enumerate() {
                out.push(vec![Piece {
                    path: &file.path,
                    content: part,
                    part: Some(u32::try_from(k + 1).unwrap_or(u32::MAX)),
                }]);
            }
            continue;
        }
        if size + weight > limit && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            size = 0;
        }
        size += weight;
        current.push(Piece {
            path: &file.path,
            content: &file.content,
            part: None,
        });
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn parts(text: &str, limit: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > limit {
        let mut cut = limit;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        out.push(head);
        rest = tail;
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> File {
        File {
            path: path.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn reads_text_files_by_relative_path_and_leaves_hidden_ones() {
        let dir = std::env::temp_dir().join(format!("romulus-cli-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("20240101_init")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(
            dir.join("20240101_init/migration.sql"),
            "CREATE TABLE a ();",
        )
        .unwrap();
        fs::write(dir.join("migration_lock.toml"), "provider = \"postgresql\"").unwrap();
        fs::write(dir.join(".DS_Store"), [0u8, 159]).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref").unwrap();
        fs::write(dir.join("diagram.png"), [0u8, 159, 146, 150]).unwrap();
        let (files, skipped) = read(&dir).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["20240101_init/migration.sql", "migration_lock.toml"]
        );
        assert_eq!(skipped, ["diagram.png"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_or_empty_folder_says_so() {
        let missing = std::env::temp_dir().join("romulus-cli-no-such-folder");
        assert!(matches!(read(&missing), Err(FilesError::Io { .. })));
        let empty = std::env::temp_dir().join(format!("romulus-cli-empty-{}", std::process::id()));
        fs::create_dir_all(&empty).unwrap();
        assert!(matches!(read(&empty), Err(FilesError::Empty(_))));
        fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn batches_fit_the_limit_and_large_files_go_in_parts() {
        let files = vec![
            file("1.sql", &"a".repeat(40)),
            file("2.sql", &"b".repeat(40)),
            file("3.sql", &"é".repeat(200)),
            file("4.sql", "d"),
        ];
        let batches = batches(&files, 300);
        // 1 and 2 fit together; 3 (400 bytes) goes in two parts; 4 alone.
        assert_eq!(batches.len(), 4, "{batches:?}");
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[3].len(), 1);
        let parts: Vec<_> = batches
            .iter()
            .flatten()
            .filter(|p| p.path == "3.sql")
            .collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part, Some(1));
        assert_eq!(parts[1].part, Some(2));
        assert_eq!(
            format!("{}{}", parts[0].content, parts[1].content),
            "é".repeat(200)
        );
        assert!(parts.iter().all(|p| p.content.len() <= 300));
    }
}
