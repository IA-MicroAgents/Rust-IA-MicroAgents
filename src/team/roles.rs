use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleProfile {
    pub role: String,
    pub model_route: Option<String>,
    pub allowed_skills: Option<Vec<String>>,
    pub prompt_overlay: Option<String>,
}

pub type RoleProfiles = HashMap<String, RoleProfile>;

pub fn load_profiles(path: Option<&Path>) -> AppResult<RoleProfiles> {
    let Some(path) = path else {
        return Ok(HashMap::new());
    };

    if !path.exists() {
        return Err(AppError::Config(format!(
            "FERRUM_SUBAGENT_PROFILE_PATH does not exist: {}",
            path.display()
        )));
    }

    let mut out = HashMap::new();
    for entry in std::fs::read_dir(path)
        .map_err(|e| AppError::Config(format!("cannot read profile dir: {e}")))?
        .flatten()
    {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yaml" && ext != "yml" {
            continue;
        }

        let raw = std::fs::read_to_string(&p)
            .map_err(|e| AppError::Config(format!("cannot read {}: {e}", p.display())))?;
        let profile: RoleProfile = serde_yaml::from_str(&raw)
            .map_err(|e| AppError::Config(format!("invalid role profile {}: {e}", p.display())))?;
        out.insert(profile.role.clone(), profile);
    }

    Ok(out)
}
