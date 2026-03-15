pub fn deterministic_acceptance_score(
    artifact: &str,
    acceptance_criteria: &[String],
    task_description: &str,
) -> f64 {
    if artifact.trim().is_empty() {
        return 0.0;
    }

    let mut score = 0.3;
    if artifact.len() >= 60 {
        score += 0.2;
    }

    let artifact_lower = artifact.to_lowercase();
    if task_description
        .split_whitespace()
        .take(6)
        .any(|t| artifact_lower.contains(&t.to_lowercase()))
    {
        score += 0.2;
    }

    let criteria_hits = acceptance_criteria
        .iter()
        .filter(|c| {
            c.split_whitespace()
                .take(4)
                .any(|w| artifact_lower.contains(&w.to_lowercase()))
        })
        .count();

    if !acceptance_criteria.is_empty() {
        score += 0.3 * (criteria_hits as f64 / acceptance_criteria.len() as f64);
    }

    score.clamp(0.0, 1.0)
}
