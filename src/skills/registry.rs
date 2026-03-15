use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use notify::{Config as NotifyConfig, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use walkdir::WalkDir;

use crate::errors::{AppError, AppResult};

use super::manifest::{SkillDoc, SkillKind, SkillManifest};

#[derive(Debug, Clone)]
pub struct SkillDefinition {
    pub manifest: SkillManifest,
    pub sections: HashMap<String, String>,
    pub folder: PathBuf,
}

#[derive(Clone)]
pub struct SkillRegistry {
    root: PathBuf,
    skills: Arc<RwLock<HashMap<String, SkillDefinition>>>,
}

impl SkillRegistry {
    pub fn load(root: PathBuf) -> AppResult<Self> {
        let skills = load_all_skills(&root)?;
        Ok(Self {
            root,
            skills: Arc::new(RwLock::new(skills)),
        })
    }

    pub fn lint(root: &Path) -> AppResult<Vec<SkillDefinition>> {
        let skills = load_all_skills(root)?;
        Ok(skills.into_values().collect())
    }

    pub fn get(&self, skill_name: &str) -> Option<SkillDefinition> {
        self.skills.read().get(skill_name).cloned()
    }

    pub fn list(&self) -> Vec<SkillDefinition> {
        self.skills.read().values().cloned().collect()
    }

    pub fn count(&self) -> usize {
        self.skills.read().len()
    }

    pub fn spawn_watcher(&self) -> AppResult<()> {
        let root = self.root.clone();
        let state = self.skills.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let mut watcher: RecommendedWatcher = RecommendedWatcher::new(
            move |result| {
                let _ = tx.send(result);
            },
            NotifyConfig::default(),
        )
        .map_err(|e| AppError::Skill(format!("watcher init failed: {e}")))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| AppError::Skill(format!("watch failed for {}: {e}", root.display())))?;

        tokio::spawn(async move {
            let _watcher = watcher;
            while let Some(event) = rx.recv().await {
                match event {
                    Ok(ev) => {
                        let should_reload = matches!(
                            ev.kind,
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                        );
                        if !should_reload {
                            continue;
                        }
                        match load_all_skills(&root) {
                            Ok(fresh) => {
                                *state.write() = fresh;
                                info!(skills = state.read().len(), "skills reloaded");
                            }
                            Err(err) => {
                                warn!(error = %err, "skills reload rejected; keeping last known good");
                            }
                        }
                    }
                    Err(err) => {
                        error!(error = %err, "skills watcher error");
                    }
                }
            }
        });

        Ok(())
    }
}

fn load_all_skills(root: &Path) -> AppResult<HashMap<String, SkillDefinition>> {
    if !root.exists() {
        return Err(AppError::Skill(format!(
            "skills directory {} does not exist",
            root.display()
        )));
    }

    let mut skills: HashMap<String, SkillDefinition> = HashMap::new();
    for entry in WalkDir::new(root)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .flatten()
    {
        if entry.file_name() != "SKILL.md" {
            continue;
        }

        let path = entry.into_path();
        let skip_dev_skill = path
            .parent()
            .and_then(|folder| folder.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('_'));
        if skip_dev_skill {
            continue;
        }
        let doc = SkillDoc::parse_file(&path)?;
        let folder = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());

        if let Some(existing) = skills.get(&doc.frontmatter.name) {
            return Err(AppError::Skill(format!(
                "duplicate skill '{}' between {} and {}",
                doc.frontmatter.name,
                existing.folder.display(),
                folder.display()
            )));
        }

        if doc.frontmatter.kind == SkillKind::Command
            && !Path::new(&doc.frontmatter.entrypoint).is_absolute()
        {
            let candidate = folder.join(&doc.frontmatter.entrypoint);
            if !candidate.exists() {
                return Err(AppError::Skill(format!(
                    "command skill {} entrypoint '{}' must be absolute or exist relative to skill folder",
                    doc.frontmatter.name,
                    doc.frontmatter.entrypoint
                )));
            }
        }

        skills.insert(
            doc.frontmatter.name.clone(),
            SkillDefinition {
                manifest: doc.frontmatter,
                sections: doc.sections,
                folder,
            },
        );
    }

    Ok(skills)
}
