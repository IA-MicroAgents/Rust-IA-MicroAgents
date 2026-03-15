use std::{path::PathBuf, sync::Arc};

use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::errors::{AppError, AppResult};

use super::{compiler::SystemIdentity, schema::IdentityDoc};

#[derive(Clone)]
pub struct IdentityManager {
    path: PathBuf,
    current: Arc<RwLock<SystemIdentity>>,
}

impl IdentityManager {
    pub fn load(path: PathBuf) -> AppResult<Self> {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| AppError::Identity(format!("failed reading {}: {e}", path.display())))?;
        let parsed = IdentityDoc::parse(&content)?;
        let compiled = SystemIdentity::compile(parsed);

        Ok(Self {
            path,
            current: Arc::new(RwLock::new(compiled)),
        })
    }

    pub fn get(&self) -> SystemIdentity {
        self.current.read().clone()
    }

    pub fn lint(path: PathBuf) -> AppResult<SystemIdentity> {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| AppError::Identity(format!("failed reading {}: {e}", path.display())))?;
        let parsed = IdentityDoc::parse(&content)?;
        Ok(SystemIdentity::compile(parsed))
    }

    pub fn spawn_watcher(&self) -> AppResult<()> {
        let path = self.path.clone();
        let state = self.current.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut watcher: RecommendedWatcher = RecommendedWatcher::new(
            move |result| {
                let _ = tx.send(result);
            },
            NotifyConfig::default(),
        )
        .map_err(|e| AppError::Identity(format!("watcher init failed: {e}")))?;

        watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|e| AppError::Identity(format!("watch failed for {}: {e}", path.display())))?;

        tokio::spawn(async move {
            let _watcher = watcher;
            while let Some(event) = rx.recv().await {
                match event {
                    Ok(ev) => {
                        let should_reload =
                            matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_));
                        if !should_reload {
                            continue;
                        }
                        match std::fs::read_to_string(&path) {
                            Ok(content) => match IdentityDoc::parse(&content) {
                                Ok(doc) => {
                                    let compiled = SystemIdentity::compile(doc);
                                    *state.write() = compiled;
                                    info!(path = %path.display(), "identity reloaded");
                                }
                                Err(err) => {
                                    warn!(error = %err, "identity reload rejected; keeping last known good");
                                }
                            },
                            Err(err) => {
                                error!(error = %err, path = %path.display(), "failed to read identity during reload");
                            }
                        }
                    }
                    Err(err) => {
                        warn!(error = %err, "identity watcher event error");
                    }
                }
            }
        });

        Ok(())
    }
}
