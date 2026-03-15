use std::fs;

use crate::errors::{AppError, AppResult};

pub const POSTGRES_MIGRATION_001: &str = include_str!("../../migrations/postgres/0001_init.sql");

pub fn load_postgres_migrations_from_disk() -> AppResult<Vec<String>> {
    load_migrations_from_dir("migrations/postgres", POSTGRES_MIGRATION_001)
}

fn load_migrations_from_dir(path: &str, fallback: &str) -> AppResult<Vec<String>> {
    let mut entries = fs::read_dir(path)
        .map_err(|e| AppError::Storage(format!("failed to read migrations dir {path}: {e}")))?
        .flatten()
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.path());

    let mut out = Vec::new();
    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|e| AppError::Storage(format!("failed reading migration file type: {e}")))?;
        if !file_type.is_file() {
            continue;
        }
        let content = fs::read_to_string(entry.path()).map_err(|e| {
            AppError::Storage(format!(
                "failed reading migration {}: {e}",
                entry.path().display()
            ))
        })?;
        out.push(content);
    }
    if out.is_empty() {
        out.push(fallback.to_string());
    }
    Ok(out)
}
