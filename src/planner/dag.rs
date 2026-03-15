use std::collections::{HashMap, HashSet};

use petgraph::{algo::toposort, graph::DiGraph, visit::EdgeRef};

use super::plan::{PlanTask, TaskState};

pub fn ready_task_ids(tasks: &[PlanTask]) -> Vec<String> {
    let states = tasks
        .iter()
        .map(|task| (task.id.clone(), task.state.clone()))
        .collect::<HashMap<_, _>>();

    tasks
        .iter()
        .filter(|task| {
            matches!(
                task.state,
                TaskState::Pending | TaskState::Ready | TaskState::Retrying
            )
        })
        .filter(|task| {
            task.dependencies.iter().all(|dep| {
                matches!(
                    states.get(dep),
                    Some(TaskState::Accepted) | Some(TaskState::Completed)
                )
            })
        })
        .map(|task| task.id.clone())
        .collect()
}

pub fn topological_levels(tasks: &[PlanTask]) -> Vec<Vec<String>> {
    let (graph, nodes) = build_graph(tasks);
    let ordering = toposort(&graph, None).unwrap_or_default();
    if ordering.is_empty() {
        return Vec::new();
    }

    let reverse_nodes = nodes
        .iter()
        .map(|(task_id, idx)| (*idx, task_id.clone()))
        .collect::<HashMap<_, _>>();
    let mut levels_by_index = HashMap::new();

    for node in ordering {
        let level = graph
            .edges_directed(node, petgraph::Incoming)
            .filter_map(|edge| levels_by_index.get(&edge.source()).copied())
            .max()
            .map(|value| value + 1)
            .unwrap_or(0);
        levels_by_index.insert(node, level);
    }

    let mut grouped = HashMap::<usize, Vec<String>>::new();
    for (node, level) in levels_by_index {
        if let Some(task_id) = reverse_nodes.get(&node) {
            grouped.entry(level).or_default().push(task_id.clone());
        }
    }

    let mut levels = grouped.into_iter().collect::<Vec<_>>();
    levels.sort_by_key(|(level, _)| *level);
    levels
        .into_iter()
        .map(|(_, mut ids)| {
            ids.sort();
            ids
        })
        .collect()
}

pub fn has_cycle(tasks: &[PlanTask]) -> bool {
    let (graph, _) = build_graph(tasks);
    toposort(&graph, None).is_err()
}

fn build_graph(
    tasks: &[PlanTask],
) -> (DiGraph<(), ()>, HashMap<String, petgraph::graph::NodeIndex>) {
    let mut graph = DiGraph::<(), ()>::new();
    let mut nodes = HashMap::new();

    for task in tasks {
        let node = graph.add_node(());
        nodes.insert(task.id.clone(), node);
    }

    for task in tasks {
        let Some(task_node) = nodes.get(&task.id).copied() else {
            continue;
        };
        for dep in &task.dependencies {
            if let Some(dep_node) = nodes.get(dep).copied() {
                graph.add_edge(dep_node, task_node, ());
            }
        }
    }

    (graph, nodes)
}

pub fn reachable_dependencies(tasks: &[PlanTask], task_id: &str) -> Vec<String> {
    let task_map = tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let mut visited = HashSet::new();
    let mut stack = vec![task_id.to_string()];
    let mut deps = Vec::new();

    while let Some(current) = stack.pop() {
        let Some(task) = task_map.get(current.as_str()) else {
            continue;
        };
        for dep in &task.dependencies {
            if visited.insert(dep.clone()) {
                deps.push(dep.clone());
                stack.push(dep.clone());
            }
        }
    }

    deps.sort();
    deps
}

#[cfg(test)]
mod tests {
    use crate::planner::plan::{PlanTask, TaskState};

    use super::{has_cycle, reachable_dependencies, ready_task_ids, topological_levels};

    fn task(id: &str, deps: &[&str], state: TaskState) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            title: id.to_string(),
            description: id.to_string(),
            depth: 1,
            dependencies: deps.iter().map(|x| x.to_string()).collect(),
            acceptance_criteria: vec![],
            candidate_role: None,
            model_route: None,
            route_key: "fast_text".to_string(),
            resolved_model: "model-fast".to_string(),
            requires_live_data: false,
            evidence_inputs: vec![],
            analysis_track: "general_analysis".to_string(),
            expected_artifact: "text".to_string(),
            estimated_cost_usd: 0.01,
            estimated_ms: 100,
            max_latency_ms: 250,
            state,
            attempts: 0,
            review_loops: 0,
        }
    }

    #[test]
    fn resolves_ready_tasks_with_dependencies() {
        let tasks = vec![
            task("a", &[], TaskState::Accepted),
            task("b", &["a"], TaskState::Pending),
            task("c", &["b"], TaskState::Pending),
        ];
        let ready = ready_task_ids(&tasks);
        assert_eq!(ready, vec!["b"]);
    }

    #[test]
    fn computes_topological_levels() {
        let tasks = vec![
            task("a", &[], TaskState::Pending),
            task("b", &["a"], TaskState::Pending),
            task("c", &["a"], TaskState::Pending),
        ];
        let levels = topological_levels(&tasks);
        assert_eq!(levels[0], vec!["a"]);
        assert_eq!(levels[1].len(), 2);
    }

    #[test]
    fn detects_cycles() {
        let tasks = vec![
            task("a", &["c"], TaskState::Pending),
            task("b", &["a"], TaskState::Pending),
            task("c", &["b"], TaskState::Pending),
        ];
        assert!(has_cycle(&tasks));
    }

    #[test]
    fn collects_transitive_dependencies() {
        let tasks = vec![
            task("a", &[], TaskState::Pending),
            task("b", &["a"], TaskState::Pending),
            task("c", &["b"], TaskState::Pending),
        ];
        assert_eq!(reachable_dependencies(&tasks, "c"), vec!["a", "b"]);
    }
}
