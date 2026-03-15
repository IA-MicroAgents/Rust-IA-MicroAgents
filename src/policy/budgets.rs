use std::time::{Duration, Instant};

use crate::identity::schema::IdentityBudgets;

#[derive(Debug, Clone)]
pub struct TurnBudget {
    started_at: Instant,
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub max_turn_cost_usd: f64,
    pub timeout: Duration,
    pub used_steps: u32,
    pub used_tool_calls: u32,
    pub used_cost_usd: f64,
}

impl TurnBudget {
    pub fn from_identity(budgets: &IdentityBudgets) -> Self {
        Self {
            started_at: Instant::now(),
            max_steps: budgets.max_steps,
            max_tool_calls: budgets.max_tool_calls,
            max_turn_cost_usd: budgets.max_turn_cost_usd,
            timeout: Duration::from_millis(budgets.timeout_ms),
            used_steps: 0,
            used_tool_calls: 0,
            used_cost_usd: 0.0,
        }
    }

    pub fn consume_step(&mut self) -> bool {
        self.used_steps += 1;
        self.used_steps <= self.max_steps && !self.is_timed_out()
    }

    pub fn consume_tool_call(&mut self) -> bool {
        self.used_tool_calls += 1;
        self.used_tool_calls <= self.max_tool_calls && !self.is_timed_out()
    }

    pub fn add_cost(&mut self, cost: f64) -> bool {
        self.used_cost_usd += cost;
        self.used_cost_usd <= self.max_turn_cost_usd && !self.is_timed_out()
    }

    pub fn is_timed_out(&self) -> bool {
        self.started_at.elapsed() > self.timeout
    }
}

#[cfg(test)]
mod tests {
    use crate::identity::schema::IdentityBudgets;

    use super::TurnBudget;

    #[test]
    fn enforces_limits() {
        let mut budget = TurnBudget::from_identity(&IdentityBudgets {
            max_steps: 1,
            max_turn_cost_usd: 0.01,
            max_input_tokens: 1_000,
            max_output_tokens: 500,
            max_tool_calls: 1,
            timeout_ms: 10_000,
        });

        assert!(budget.consume_step());
        assert!(!budget.consume_step());
        assert!(budget.consume_tool_call());
        assert!(!budget.consume_tool_call());
        assert!(budget.add_cost(0.005));
        assert!(!budget.add_cost(0.01));
    }
}
