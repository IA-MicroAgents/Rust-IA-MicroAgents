use std::{collections::HashMap, env, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    errors::{AppError, AppResult},
    identity::{compiler::SystemIdentity, schema::IdentityPermissions},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentMode {
    Generalist,
    Specialist,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PerformancePolicy {
    Fast,
    BalancedFast,
    MaxQuality,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTier {
    Conservative,
    Standard,
    Aggressive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub team_size: usize,
    pub max_parallel_tasks: usize,
    pub allow_ephemeral_subagents: bool,
    pub max_ephemeral_subagents: usize,
    pub subagent_mode: SubagentMode,
    pub subagent_roleset: Vec<String>,
    pub subagent_profile_path: Option<PathBuf>,
    pub supervisor_review_interval_ms: u64,
    pub max_review_loops_per_task: u32,
    pub max_task_retries: u32,
    pub plan_max_tasks: usize,
    pub plan_max_depth: usize,
    pub require_final_review: bool,
    pub progress_updates_enabled: bool,
    pub progress_update_threshold_ms: u64,
    pub performance_policy: PerformancePolicy,
    pub planner_aggressiveness: u8,
    pub max_escalation_tier: EscalationTier,
    pub typing_delay_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrincipalSkillConfig {
    pub allowed_skills: Vec<String>,
    pub denied_skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoleSpecialization {
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    #[serde(default)]
    pub model_route: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubagentSpecialization {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    #[serde(default)]
    pub model_route: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamRuntimeSettings {
    pub team_size: usize,
    pub max_parallel_tasks: usize,
    pub allow_ephemeral_subagents: bool,
    pub max_ephemeral_subagents: usize,
    pub subagent_mode: SubagentMode,
    pub subagent_roleset: Vec<String>,
    pub subagent_profile_path: Option<PathBuf>,
    pub supervisor_review_interval_ms: u64,
    pub max_review_loops_per_task: u32,
    pub max_task_retries: u32,
    pub plan_max_tasks: usize,
    pub plan_max_depth: usize,
    pub require_final_review: bool,
    pub progress_updates_enabled: bool,
    pub progress_update_threshold_ms: u64,
    pub performance_policy: PerformancePolicy,
    pub planner_aggressiveness: u8,
    pub max_escalation_tier: EscalationTier,
    pub typing_delay_ms: u64,
    pub principal_skills: PrincipalSkillConfig,
    pub role_specializations: HashMap<String, RoleSpecialization>,
    pub subagent_specializations: HashMap<String, SubagentSpecialization>,
}

impl TeamConfig {
    pub fn from_env() -> AppResult<Self> {
        let team_size = usize_var("FERRUM_TEAM_SIZE", 4);
        let max_parallel_tasks = usize_var("FERRUM_MAX_PARALLEL_TASKS", team_size.max(1));
        let mode = match env::var("FERRUM_SUBAGENT_MODE")
            .unwrap_or_else(|_| "generalist".to_string())
            .as_str()
        {
            "generalist" => SubagentMode::Generalist,
            "specialist" => SubagentMode::Specialist,
            other => {
                return Err(AppError::Config(format!(
                    "FERRUM_SUBAGENT_MODE invalid value: {other}"
                )));
            }
        };

        let roleset = env::var("FERRUM_SUBAGENT_ROLESET")
            .unwrap_or_else(|_| "researcher,implementer,verifier,integrator".to_string())
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        if roleset.is_empty() {
            return Err(AppError::Config(
                "FERRUM_SUBAGENT_ROLESET must define at least one role".to_string(),
            ));
        }

        let cfg = Self {
            team_size,
            max_parallel_tasks,
            allow_ephemeral_subagents: bool_var("FERRUM_ALLOW_EPHEMERAL_SUBAGENTS", true),
            max_ephemeral_subagents: usize_var(
                "FERRUM_MAX_EPHEMERAL_SUBAGENTS",
                team_size.saturating_mul(2).max(2),
            ),
            subagent_mode: mode,
            subagent_roleset: roleset,
            subagent_profile_path: env::var("FERRUM_SUBAGENT_PROFILE_PATH")
                .ok()
                .map(PathBuf::from),
            supervisor_review_interval_ms: u64_var("FERRUM_SUPERVISOR_REVIEW_INTERVAL_MS", 1000),
            max_review_loops_per_task: u32_var("FERRUM_MAX_REVIEW_LOOPS_PER_TASK", 3),
            max_task_retries: u32_var("FERRUM_MAX_TASK_RETRIES", 2),
            plan_max_tasks: usize_var("FERRUM_PLAN_MAX_TASKS", 8),
            plan_max_depth: usize_var("FERRUM_PLAN_MAX_DEPTH", 3),
            require_final_review: bool_var("FERRUM_REQUIRE_FINAL_REVIEW", true),
            progress_updates_enabled: bool_var("FERRUM_PROGRESS_UPDATES_ENABLED", true),
            progress_update_threshold_ms: u64_var("FERRUM_PROGRESS_UPDATE_THRESHOLD_MS", 8000),
            performance_policy: match env::var("FERRUM_PERFORMANCE_POLICY")
                .unwrap_or_else(|_| "balanced_fast".to_string())
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "fast" => PerformancePolicy::Fast,
                "balanced_fast" | "balanced-fast" | "balanced" => PerformancePolicy::BalancedFast,
                "max_quality" | "max-quality" | "quality" => PerformancePolicy::MaxQuality,
                other => {
                    return Err(AppError::Config(format!(
                        "FERRUM_PERFORMANCE_POLICY invalid value: {other}"
                    )));
                }
            },
            planner_aggressiveness: u8_var("FERRUM_PLANNER_AGGRESSIVENESS", 60),
            max_escalation_tier: match env::var("FERRUM_MAX_ESCALATION_TIER")
                .unwrap_or_else(|_| "standard".to_string())
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "conservative" => EscalationTier::Conservative,
                "standard" => EscalationTier::Standard,
                "aggressive" => EscalationTier::Aggressive,
                other => {
                    return Err(AppError::Config(format!(
                        "FERRUM_MAX_ESCALATION_TIER invalid value: {other}"
                    )));
                }
            },
            typing_delay_ms: u64_var("TELEGRAM_TYPING_DELAY_MS", 800),
        };

        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> AppResult<()> {
        if self.team_size == 0 {
            return Err(AppError::Config(
                "FERRUM_TEAM_SIZE must be >= 1".to_string(),
            ));
        }
        if self.max_parallel_tasks == 0 {
            return Err(AppError::Config(
                "FERRUM_MAX_PARALLEL_TASKS must be >= 1".to_string(),
            ));
        }
        let max_capacity = if self.allow_ephemeral_subagents {
            self.team_size
                .saturating_add(self.max_ephemeral_subagents)
                .max(self.team_size)
        } else {
            self.team_size
        };
        if self.max_parallel_tasks > max_capacity {
            return Err(AppError::Config(
                "FERRUM_MAX_PARALLEL_TASKS exceeds available persistent + ephemeral capacity"
                    .to_string(),
            ));
        }
        if self.max_review_loops_per_task == 0 || self.max_task_retries == 0 {
            return Err(AppError::Config(
                "review loops and task retries must be >= 1".to_string(),
            ));
        }
        if self.plan_max_tasks == 0 || self.plan_max_depth == 0 {
            return Err(AppError::Config("plan limits must be >= 1".to_string()));
        }
        if self.planner_aggressiveness > 100 {
            return Err(AppError::Config(
                "FERRUM_PLANNER_AGGRESSIVENESS must be between 0 and 100".to_string(),
            ));
        }
        Ok(())
    }
}

impl TeamRuntimeSettings {
    pub fn from_bootstrap(cfg: &TeamConfig, _identity: &SystemIdentity) -> Self {
        Self {
            team_size: cfg.team_size,
            max_parallel_tasks: cfg.max_parallel_tasks,
            allow_ephemeral_subagents: cfg.allow_ephemeral_subagents,
            max_ephemeral_subagents: cfg.max_ephemeral_subagents,
            subagent_mode: cfg.subagent_mode.clone(),
            subagent_roleset: cfg.subagent_roleset.clone(),
            subagent_profile_path: cfg.subagent_profile_path.clone(),
            supervisor_review_interval_ms: cfg.supervisor_review_interval_ms,
            max_review_loops_per_task: cfg.max_review_loops_per_task,
            max_task_retries: cfg.max_task_retries,
            plan_max_tasks: cfg.plan_max_tasks,
            plan_max_depth: cfg.plan_max_depth,
            require_final_review: cfg.require_final_review,
            progress_updates_enabled: cfg.progress_updates_enabled,
            progress_update_threshold_ms: cfg.progress_update_threshold_ms,
            performance_policy: cfg.performance_policy.clone(),
            planner_aggressiveness: cfg.planner_aggressiveness,
            max_escalation_tier: cfg.max_escalation_tier.clone(),
            typing_delay_ms: cfg.typing_delay_ms,
            principal_skills: PrincipalSkillConfig {
                allowed_skills: vec!["*".to_string()],
                denied_skills: vec![],
            },
            role_specializations: HashMap::new(),
            subagent_specializations: HashMap::new(),
        }
        .normalized_for_automatic_assignment()
    }

    pub fn as_team_config(&self) -> TeamConfig {
        TeamConfig {
            team_size: self.team_size,
            max_parallel_tasks: self.max_parallel_tasks,
            allow_ephemeral_subagents: self.allow_ephemeral_subagents,
            max_ephemeral_subagents: self.max_ephemeral_subagents,
            subagent_mode: self.subagent_mode.clone(),
            subagent_roleset: self.subagent_roleset.clone(),
            subagent_profile_path: self.subagent_profile_path.clone(),
            supervisor_review_interval_ms: self.supervisor_review_interval_ms,
            max_review_loops_per_task: self.max_review_loops_per_task,
            max_task_retries: self.max_task_retries,
            plan_max_tasks: self.plan_max_tasks,
            plan_max_depth: self.plan_max_depth,
            require_final_review: self.require_final_review,
            progress_updates_enabled: self.progress_updates_enabled,
            progress_update_threshold_ms: self.progress_update_threshold_ms,
            performance_policy: self.performance_policy.clone(),
            planner_aggressiveness: self.planner_aggressiveness,
            max_escalation_tier: self.max_escalation_tier.clone(),
            typing_delay_ms: self.typing_delay_ms,
        }
    }

    pub fn validate(&self) -> AppResult<()> {
        self.as_team_config().validate()?;
        if self.principal_skills.allowed_skills.is_empty() {
            return Err(AppError::Config(
                "principal skill set cannot be empty".to_string(),
            ));
        }
        if self.subagent_roleset.is_empty() {
            return Err(AppError::Config(
                "subagent roleset cannot be empty".to_string(),
            ));
        }
        Ok(())
    }

    pub fn normalized_for_automatic_assignment(mut self) -> Self {
        self.principal_skills = PrincipalSkillConfig {
            allowed_skills: vec!["*".to_string()],
            denied_skills: vec![],
        };
        self.role_specializations.clear();
        self.subagent_specializations.clear();
        self
    }

    pub fn effective_permissions(&self, defaults: &IdentityPermissions) -> IdentityPermissions {
        let _ = defaults;
        IdentityPermissions {
            allowed_skills: vec!["*".to_string()],
            denied_skills: vec![],
        }
    }
}

fn bool_var(name: &str, default: bool) -> bool {
    let raw = match std::env::var(name) {
        Ok(value) => value,
        Err(_) => return default,
    };
    match raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "y" | "on" => true,
        "0" | "false" | "no" | "n" | "off" => false,
        _ => default,
    }
}

fn usize_var(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default)
}

fn u64_var(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}

fn u32_var(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .unwrap_or(default)
}

fn u8_var(name: &str, default: u8) -> u8 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<u8>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        EscalationTier, PerformancePolicy, PrincipalSkillConfig, RoleSpecialization, SubagentMode,
        TeamConfig, TeamRuntimeSettings,
    };

    #[test]
    fn validates_team_config() {
        let cfg = TeamConfig {
            team_size: 3,
            max_parallel_tasks: 2,
            allow_ephemeral_subagents: true,
            max_ephemeral_subagents: 4,
            subagent_mode: SubagentMode::Generalist,
            subagent_roleset: vec!["researcher".to_string()],
            subagent_profile_path: None,
            supervisor_review_interval_ms: 500,
            max_review_loops_per_task: 2,
            max_task_retries: 2,
            plan_max_tasks: 6,
            plan_max_depth: 3,
            require_final_review: true,
            progress_updates_enabled: true,
            progress_update_threshold_ms: 3000,
            performance_policy: PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: EscalationTier::Standard,
            typing_delay_ms: 800,
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn runtime_settings_validate_specializations() {
        let settings = TeamRuntimeSettings {
            team_size: 2,
            max_parallel_tasks: 2,
            allow_ephemeral_subagents: true,
            max_ephemeral_subagents: 2,
            subagent_mode: SubagentMode::Specialist,
            subagent_roleset: vec!["researcher".to_string(), "verifier".to_string()],
            subagent_profile_path: None,
            supervisor_review_interval_ms: 1000,
            max_review_loops_per_task: 3,
            max_task_retries: 2,
            plan_max_tasks: 8,
            plan_max_depth: 3,
            require_final_review: true,
            progress_updates_enabled: true,
            progress_update_threshold_ms: 5000,
            performance_policy: PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: EscalationTier::Standard,
            typing_delay_ms: 800,
            principal_skills: PrincipalSkillConfig {
                allowed_skills: vec!["*".to_string()],
                denied_skills: vec![],
            },
            role_specializations: HashMap::from([(
                "researcher".to_string(),
                RoleSpecialization {
                    allowed_skills: vec!["memory.search".to_string()],
                    model_route: None,
                },
            )]),
            subagent_specializations: HashMap::new(),
        }
        .normalized_for_automatic_assignment();

        assert!(settings.validate().is_ok());
        assert_eq!(
            settings.principal_skills.allowed_skills,
            vec!["*".to_string()]
        );
        assert!(settings.role_specializations.is_empty());
    }
}
