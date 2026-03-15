use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactWrite {
    pub key: String,
    pub value: String,
    pub confidence: f64,
}
