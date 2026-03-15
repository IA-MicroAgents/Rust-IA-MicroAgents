use askama::Template;

#[derive(Template)]
#[template(path = "dashboard_index.html")]
pub struct DashboardIndexTemplate {
    pub runtime_state: String,
    pub identity_id: String,
    pub database_backend: String,
    pub cache_backend: String,
    pub team_size: usize,
    pub loaded_skills: usize,
    pub queue_depth: i64,
    pub total_cost_usd: f64,
    pub active_channel: String,
    pub telegram_enabled: bool,
    pub telegram_bot_username: String,
    pub telegram_bot_link: String,
    pub effective_parallel_limit: usize,
    pub configured_team_size: usize,
}

#[derive(Template)]
#[template(path = "dashboard_conversation.html")]
pub struct DashboardConversationTemplate {
    pub conversation_id: i64,
    pub inbound: String,
    pub inbound_len: usize,
    pub final_answer: String,
    pub final_answer_len: usize,
}
