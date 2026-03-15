use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRoute {
    DirectReply,
    ToolUse,
    PlanThenAct,
    Ignore,
    AskClarification,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryWrite {
    pub key: String,
    pub value: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OrchestrationDecision {
    pub route: DecisionRoute,
    pub assistant_reply: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub memory_writes: Vec<MemoryWrite>,
    pub should_summarize: bool,
    pub confidence: f64,
    pub safe_to_send: bool,
}

impl OrchestrationDecision {
    pub fn safe_fallback(message: &str) -> Self {
        Self {
            route: DecisionRoute::AskClarification,
            assistant_reply: message.to_string(),
            tool_calls: Vec::new(),
            memory_writes: Vec::new(),
            should_summarize: false,
            confidence: 0.1,
            safe_to_send: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmPlanTask {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub candidate_role: Option<String>,
    #[serde(default)]
    pub model_route: Option<String>,
    #[serde(default)]
    pub requires_live_data: bool,
    #[serde(default)]
    pub evidence_inputs: Vec<String>,
    #[serde(default)]
    pub analysis_track: String,
    pub expected_artifact: String,
    pub estimated_cost_usd: f64,
    pub estimated_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionPlanContract {
    pub goal: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<LlmPlanTask>,
    #[serde(default)]
    pub parallelizable_groups: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskPacket {
    pub task_id: String,
    pub role: String,
    pub prompt: String,
    #[serde(default)]
    pub input_context: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewDecisionContract {
    pub action: String,
    pub score: f64,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationBundle {
    pub accepted_task_ids: Vec<String>,
    pub synthesis: String,
    pub open_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FinalDelivery {
    pub assistant_reply: String,
    pub safe_to_send: bool,
    pub confidence: f64,
}
