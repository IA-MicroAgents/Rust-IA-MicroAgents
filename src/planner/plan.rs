use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Ready,
    Assigned,
    Running,
    Blocked,
    WaitingReview,
    Accepted,
    Rejected,
    Retrying,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub depth: u32,
    pub dependencies: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub candidate_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route: Option<String>,
    #[serde(default)]
    pub route_key: String,
    #[serde(default)]
    pub resolved_model: String,
    #[serde(default)]
    pub requires_live_data: bool,
    #[serde(default)]
    pub evidence_inputs: Vec<String>,
    #[serde(default)]
    pub analysis_track: String,
    pub expected_artifact: String,
    pub estimated_cost_usd: f64,
    pub estimated_ms: u64,
    #[serde(default)]
    pub max_latency_ms: u64,
    pub state: TaskState,
    pub attempts: u32,
    pub review_loops: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionPlan {
    pub id: String,
    pub conversation_id: i64,
    pub goal: String,
    pub assumptions: Vec<String>,
    pub risks: Vec<String>,
    pub tasks: Vec<PlanTask>,
    pub parallelizable_groups: Vec<Vec<String>>,
    pub max_depth: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExecutionPlan {
    pub fn new(conversation_id: i64, goal: String, max_depth: u32) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            conversation_id,
            goal,
            assumptions: Vec::new(),
            risks: Vec::new(),
            tasks: Vec::new(),
            parallelizable_groups: Vec::new(),
            max_depth,
            created_at: now,
            updated_at: now,
        }
    }
}
