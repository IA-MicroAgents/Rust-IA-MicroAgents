use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::{
    errors::{AppError, AppResult},
    identity::{compiler::SystemIdentity, schema::IdentityPermissions},
    storage::Store,
    team::{
        config::{
            RoleSpecialization, SubagentMode, SubagentSpecialization, TeamConfig,
            TeamRuntimeSettings,
        },
        resources::{ResourceMonitor, ResourceSnapshot},
        roles::{load_profiles, RoleProfiles},
        subagent::{Subagent, SubagentState},
    },
};
use chrono::Utc;
use parking_lot::RwLock;

const TEAM_SETTINGS_SNAPSHOT: &str = "dashboard_team_settings";

#[derive(Debug, Clone)]
struct SubagentBlueprint {
    role: String,
    model_route: String,
    resolved_model: String,
    allowed_skills: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AcquiredSubagent {
    pub subagent: Subagent,
    pub spawned: bool,
}

#[derive(Debug, Clone)]
pub struct ReleaseOutcome {
    pub subagent: Subagent,
    pub destroyed: bool,
}

#[derive(Clone)]
pub struct TeamManager {
    config: Arc<RwLock<TeamRuntimeSettings>>,
    subagents: Arc<RwLock<Vec<Subagent>>>,
    profiles: Arc<RwLock<RoleProfiles>>,
    identity: Arc<SystemIdentity>,
    next_ephemeral_id: Arc<AtomicUsize>,
    store: Option<Store>,
    resources: ResourceMonitor,
}

impl TeamManager {
    pub async fn new(cfg: TeamConfig, identity: &SystemIdentity) -> AppResult<Self> {
        Self::new_internal(cfg, identity, None).await
    }

    pub async fn with_store(
        cfg: TeamConfig,
        identity: &SystemIdentity,
        store: Store,
    ) -> AppResult<Self> {
        Self::new_internal(cfg, identity, Some(store)).await
    }

    async fn new_internal(
        cfg: TeamConfig,
        identity: &SystemIdentity,
        store: Option<Store>,
    ) -> AppResult<Self> {
        let mut settings = TeamRuntimeSettings::from_bootstrap(&cfg, identity);
        if let Some(db) = store.as_ref() {
            if let Ok(Some(snapshot)) = db.latest_config_snapshot(TEAM_SETTINGS_SNAPSHOT).await {
                if let Ok(saved) = serde_json::from_value::<TeamRuntimeSettings>(snapshot.clone()) {
                    let saved = saved.normalized_for_automatic_assignment();
                    if saved.validate().is_ok() {
                        settings = saved;
                    }
                }
            }
        }
        settings.validate()?;
        let profiles = load_profiles(settings.subagent_profile_path.as_deref())?;
        let resources = ResourceMonitor::new(settings.team_size, settings.max_ephemeral_subagents);
        let subagents = build_subagents(&settings, identity, &profiles)?;
        Ok(Self {
            config: Arc::new(RwLock::new(settings)),
            subagents: Arc::new(RwLock::new(subagents)),
            profiles: Arc::new(RwLock::new(profiles)),
            identity: Arc::new(identity.clone()),
            next_ephemeral_id: Arc::new(AtomicUsize::new(1)),
            store,
            resources,
        })
    }

    pub fn config(&self) -> TeamConfig {
        self.config.read().as_team_config()
    }

    pub fn runtime_settings(&self) -> TeamRuntimeSettings {
        self.config.read().clone()
    }

    pub fn resource_snapshot(&self) -> ResourceSnapshot {
        self.resources.snapshot()
    }

    pub fn effective_ephemeral_capacity(&self) -> usize {
        let configured = self.config.read().max_ephemeral_subagents;
        configured.min(self.resources.snapshot().suggested_ephemeral_capacity)
    }

    pub fn effective_parallel_limit(&self) -> usize {
        let cfg = self.config.read();
        cfg.max_parallel_tasks
            .min(
                cfg.team_size
                    .saturating_add(self.effective_ephemeral_capacity()),
            )
            .max(1)
    }

    pub fn effective_principal_permissions(&self) -> IdentityPermissions {
        let cfg = self.config.read();
        cfg.effective_permissions(&self.identity.frontmatter.permissions)
    }

    pub fn list(&self) -> Vec<Subagent> {
        let mut items = self.subagents.read().clone();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        items
    }

    pub fn persistent_count(&self) -> usize {
        self.subagents
            .read()
            .iter()
            .filter(|a| !a.ephemeral)
            .count()
    }

    pub fn ephemeral_count(&self) -> usize {
        self.subagents.read().iter().filter(|a| a.ephemeral).count()
    }

    pub fn roleset(&self) -> Vec<String> {
        self.config.read().subagent_roleset.clone()
    }

    pub async fn apply_runtime_settings(
        &self,
        next: TeamRuntimeSettings,
    ) -> AppResult<TeamRuntimeSettings> {
        let next = next.normalized_for_automatic_assignment();
        next.validate()?;
        let profiles = load_profiles(next.subagent_profile_path.as_deref())?;
        if let Some(store) = &self.store {
            let payload = serde_json::to_value(&next).map_err(|e| {
                AppError::Internal(format!("team settings serialization failed: {e}"))
            })?;
            store
                .insert_config_snapshot(TEAM_SETTINGS_SNAPSHOT, Some("dashboard"), &payload)
                .await?;
        }

        {
            *self.profiles.write() = profiles;
            *self.config.write() = next.clone();
            self.resources
                .update_targets(next.team_size, next.max_ephemeral_subagents);
            let mut lock = self.subagents.write();
            self.reconcile_persistent_pool(&mut lock)?;
        }
        Ok(self.runtime_settings())
    }

    pub fn acquire_for_task(
        &self,
        task_id: &str,
        preferred_role: Option<&str>,
    ) -> Option<AcquiredSubagent> {
        let mut lock = self.subagents.write();

        if let Some(role) = preferred_role {
            if let Some(agent) = lock
                .iter_mut()
                .find(|a| a.state == SubagentState::Idle && a.role.eq_ignore_ascii_case(role))
            {
                agent.state = SubagentState::Assigned;
                agent.current_task_id = Some(task_id.to_string());
                agent.heartbeat_at = Utc::now();
                return Some(AcquiredSubagent {
                    subagent: agent.clone(),
                    spawned: false,
                });
            }

            let settings = self.config.read().clone();
            if settings.allow_ephemeral_subagents {
                let current_ephemeral = lock.iter().filter(|a| a.ephemeral).count();
                let effective_cap = self.effective_ephemeral_capacity();
                if current_ephemeral < effective_cap {
                    let blueprint = self.blueprint_for_role(Some(role));
                    let subagent = Subagent {
                        id: format!(
                            "ephemeral-{}",
                            self.next_ephemeral_id.fetch_add(1, Ordering::SeqCst)
                        ),
                        role: blueprint.role,
                        model_route: blueprint.model_route,
                        resolved_model: blueprint.resolved_model,
                        allowed_skills: blueprint.allowed_skills,
                        ephemeral: true,
                        state: SubagentState::Assigned,
                        current_task_id: Some(task_id.to_string()),
                        heartbeat_at: Utc::now(),
                        retries: 0,
                        last_review_score: 0.0,
                        last_error: None,
                    };
                    lock.push(subagent.clone());
                    return Some(AcquiredSubagent {
                        subagent,
                        spawned: true,
                    });
                }
            }

            return None;
        }

        if let Some(agent) = lock.iter_mut().find(|a| a.state == SubagentState::Idle) {
            agent.state = SubagentState::Assigned;
            agent.current_task_id = Some(task_id.to_string());
            agent.heartbeat_at = Utc::now();
            return Some(AcquiredSubagent {
                subagent: agent.clone(),
                spawned: false,
            });
        }

        let settings = self.config.read().clone();
        if settings.allow_ephemeral_subagents {
            let current_ephemeral = lock.iter().filter(|a| a.ephemeral).count();
            let effective_cap = self.effective_ephemeral_capacity();
            if current_ephemeral < effective_cap {
                let blueprint = self.blueprint_for_role(preferred_role);
                let subagent = Subagent {
                    id: format!(
                        "ephemeral-{}",
                        self.next_ephemeral_id.fetch_add(1, Ordering::SeqCst)
                    ),
                    role: blueprint.role,
                    model_route: blueprint.model_route,
                    resolved_model: blueprint.resolved_model,
                    allowed_skills: blueprint.allowed_skills,
                    ephemeral: true,
                    state: SubagentState::Assigned,
                    current_task_id: Some(task_id.to_string()),
                    heartbeat_at: Utc::now(),
                    retries: 0,
                    last_review_score: 0.0,
                    last_error: None,
                };
                lock.push(subagent.clone());
                return Some(AcquiredSubagent {
                    subagent,
                    spawned: true,
                });
            }
        }

        None
    }

    pub fn mark_running(&self, subagent_id: &str) {
        if let Some(a) = self
            .subagents
            .write()
            .iter_mut()
            .find(|a| a.id == subagent_id)
        {
            a.state = SubagentState::Running;
            a.heartbeat_at = Utc::now();
        }
    }

    pub fn heartbeat(&self, subagent_id: &str) {
        if let Some(a) = self
            .subagents
            .write()
            .iter_mut()
            .find(|a| a.id == subagent_id)
        {
            a.heartbeat_at = Utc::now();
        }
    }

    pub fn release(
        &self,
        subagent_id: &str,
        review_score: f64,
        error: Option<String>,
    ) -> Option<ReleaseOutcome> {
        let mut lock = self.subagents.write();
        let position = lock.iter().position(|a| a.id == subagent_id)?;

        if lock[position].ephemeral {
            let mut released = lock.remove(position);
            released.last_review_score = review_score;
            released.last_error = error;
            released.current_task_id = None;
            released.heartbeat_at = Utc::now();
            return Some(ReleaseOutcome {
                subagent: released,
                destroyed: true,
            });
        }

        let a = &mut lock[position];
        a.state = if error.is_some() {
            SubagentState::Failed
        } else {
            SubagentState::Idle
        };
        a.current_task_id = None;
        a.last_review_score = review_score;
        a.last_error = error;
        a.heartbeat_at = Utc::now();
        if matches!(a.state, SubagentState::Failed) {
            a.retries = a.retries.saturating_add(1);
        }
        Some(ReleaseOutcome {
            subagent: a.clone(),
            destroyed: false,
        })
    }

    pub fn force_idle(&self, subagent_id: &str) {
        let mut lock = self.subagents.write();
        if let Some(position) = lock.iter().position(|a| a.id == subagent_id) {
            if lock[position].ephemeral {
                lock.remove(position);
            } else if let Some(a) = lock.get_mut(position) {
                a.state = SubagentState::Idle;
                a.current_task_id = None;
                a.heartbeat_at = Utc::now();
            }
        }
    }

    fn blueprint_for_role(&self, preferred_role: Option<&str>) -> SubagentBlueprint {
        let settings = self.config.read().clone();
        let profiles = self.profiles.read();
        if let Some(role) = preferred_role {
            return build_blueprint(&settings, &self.identity, &profiles, role, None)
                .unwrap_or_else(|| fallback_blueprint(role));
        }

        settings
            .subagent_roleset
            .first()
            .and_then(|role| build_blueprint(&settings, &self.identity, &profiles, role, None))
            .unwrap_or_else(|| fallback_blueprint("generalist"))
    }

    fn reconcile_persistent_pool(&self, lock: &mut Vec<Subagent>) -> AppResult<()> {
        let settings = self.config.read().clone();
        let profiles = self.profiles.read();
        let target_roles = (0..settings.team_size)
            .map(|idx| settings.subagent_roleset[idx % settings.subagent_roleset.len()].clone())
            .collect::<Vec<_>>();

        for (idx, role) in target_roles.iter().enumerate() {
            let subagent_id = format!("subagent-{}", idx + 1);
            let blueprint = build_blueprint(
                &settings,
                &self.identity,
                &profiles,
                role,
                Some(&subagent_id),
            )
            .ok_or_else(|| AppError::Config(format!("missing blueprint for role {role}")))?;
            if let Some(agent) = lock
                .iter_mut()
                .find(|agent| !agent.ephemeral && agent.id == subagent_id)
            {
                if agent.current_task_id.is_none() {
                    agent.role = blueprint.role;
                    agent.model_route = blueprint.model_route;
                    agent.resolved_model = blueprint.resolved_model;
                    agent.allowed_skills = blueprint.allowed_skills;
                }
            } else {
                lock.push(Subagent {
                    id: subagent_id,
                    role: blueprint.role,
                    model_route: blueprint.model_route,
                    resolved_model: blueprint.resolved_model,
                    allowed_skills: blueprint.allowed_skills,
                    ephemeral: false,
                    state: SubagentState::Idle,
                    current_task_id: None,
                    heartbeat_at: Utc::now(),
                    retries: 0,
                    last_review_score: 0.0,
                    last_error: None,
                });
            }
        }

        let desired_ids = (1..=settings.team_size)
            .map(|idx| format!("subagent-{idx}"))
            .collect::<Vec<_>>();
        lock.retain(|agent| {
            agent.ephemeral
                || desired_ids.iter().any(|id| id == &agent.id)
                || agent.current_task_id.is_some()
        });
        lock.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(())
    }
}

fn build_subagents(
    settings: &TeamRuntimeSettings,
    identity: &SystemIdentity,
    profiles: &RoleProfiles,
) -> AppResult<Vec<Subagent>> {
    if settings.subagent_roleset.is_empty() {
        return Err(AppError::Config(
            "subagent roleset cannot be empty".to_string(),
        ));
    }

    let mut out = Vec::with_capacity(settings.team_size);
    for idx in 0..settings.team_size {
        let role = settings.subagent_roleset[idx % settings.subagent_roleset.len()].clone();
        let subagent_id = format!("subagent-{}", idx + 1);
        let blueprint = build_blueprint(settings, identity, profiles, &role, Some(&subagent_id))
            .ok_or_else(|| AppError::Config(format!("missing blueprint for role {role}")))?;

        out.push(Subagent {
            id: subagent_id,
            role: blueprint.role,
            model_route: blueprint.model_route,
            resolved_model: blueprint.resolved_model,
            allowed_skills: blueprint.allowed_skills,
            ephemeral: false,
            state: SubagentState::Idle,
            current_task_id: None,
            heartbeat_at: Utc::now(),
            retries: 0,
            last_review_score: 0.0,
            last_error: None,
        });
    }
    Ok(out)
}

fn build_blueprint(
    settings: &TeamRuntimeSettings,
    identity: &SystemIdentity,
    profiles: &RoleProfiles,
    role: &str,
    subagent_id: Option<&str>,
) -> Option<SubagentBlueprint> {
    let profile = profiles.get(role);
    let role_spec = settings.role_specializations.get(role);
    let subagent_spec = subagent_id.and_then(|id| settings.subagent_specializations.get(id));

    let resolved_role = subagent_spec
        .and_then(|spec| spec.role.clone())
        .unwrap_or_else(|| role.to_string());

    let model_route = subagent_spec
        .and_then(|spec| spec.model_route.clone())
        .or_else(|| role_spec.and_then(|spec| spec.model_route.clone()))
        .or_else(|| profile.and_then(|p| p.model_route.clone()))
        .and_then(|configured| {
            normalize_blueprint_route(&configured, &resolved_role, settings.subagent_mode.clone())
        })
        .unwrap_or_else(|| default_route_key(&resolved_role, settings.subagent_mode.clone()));
    let resolved_model = resolve_blueprint_model(identity, &model_route);

    let allowed_skills = specialization_skills(subagent_spec)
        .or_else(|| specialization_skills(role_spec))
        .or_else(|| profile.and_then(|p| p.allowed_skills.clone()))
        .unwrap_or_else(|| identity.frontmatter.permissions.allowed_skills.clone());

    Some(SubagentBlueprint {
        role: resolved_role,
        model_route,
        resolved_model,
        allowed_skills,
    })
}

fn resolve_blueprint_model(identity: &SystemIdentity, configured: &str) -> String {
    identity
        .frontmatter
        .model_routes
        .route_value(configured)
        .map(ToString::to_string)
        .unwrap_or_else(|| identity.frontmatter.model_routes.fast.clone())
}

fn normalize_blueprint_route(configured: &str, role: &str, mode: SubagentMode) -> Option<String> {
    let normalized = configured.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "fast" | "router_fast" | "classifier" => Some("router_fast".to_string()),
        "fast_text" => Some("fast_text".to_string()),
        "planner" => Some("planner".to_string()),
        "tool_use" | "tool" => Some("tool_use".to_string()),
        "reviewer" | "reviewer_fast" => Some("reviewer_fast".to_string()),
        "reviewer_strict" => Some("reviewer_strict".to_string()),
        "reasoning" | "integrator" => Some("reasoning".to_string()),
        "integrator_complex" | "integration_strong" => Some("integrator_complex".to_string()),
        "vision" | "vision_understand" => Some("vision_understand".to_string()),
        "audio" | "audio_transcribe" => Some("audio_transcribe".to_string()),
        "image_generate" | "image" => Some("image_generate".to_string()),
        _ if configured.contains('/') => Some(default_route_key(role, mode)),
        _ => None,
    }
}

fn default_route_key(role: &str, mode: SubagentMode) -> String {
    match mode {
        SubagentMode::Generalist => match role.to_ascii_lowercase().as_str() {
            "verifier" | "reviewer" | "qa" => "reviewer_fast".to_string(),
            "integrator" | "synthesizer" => "reasoning".to_string(),
            "implementer" | "operator" => "tool_use".to_string(),
            _ => "fast_text".to_string(),
        },
        SubagentMode::Specialist => match role.to_ascii_lowercase().as_str() {
            "verifier" | "reviewer" | "qa" => "reviewer_strict".to_string(),
            "integrator" | "synthesizer" => "reasoning".to_string(),
            "implementer" | "operator" => "tool_use".to_string(),
            _ => "fast_text".to_string(),
        },
    }
}

fn specialization_skills<T>(specialization: Option<&T>) -> Option<Vec<String>>
where
    T: SpecializationView,
{
    specialization.and_then(|spec| {
        if spec.allowed_skills().is_empty() {
            None
        } else {
            Some(spec.allowed_skills().to_vec())
        }
    })
}

trait SpecializationView {
    fn allowed_skills(&self) -> &[String];
}

impl SpecializationView for RoleSpecialization {
    fn allowed_skills(&self) -> &[String] {
        &self.allowed_skills
    }
}

impl SpecializationView for SubagentSpecialization {
    fn allowed_skills(&self) -> &[String] {
        &self.allowed_skills
    }
}

fn fallback_blueprint(role: &str) -> SubagentBlueprint {
    SubagentBlueprint {
        role: role.to_string(),
        model_route: "fast_text".to_string(),
        resolved_model: "openai/gpt-4o-mini".to_string(),
        allowed_skills: vec!["*".to_string()],
    }
}
