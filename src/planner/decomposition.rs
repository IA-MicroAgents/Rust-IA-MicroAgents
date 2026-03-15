use std::collections::{HashMap, HashSet};

use crate::{
    identity::compiler::SystemIdentity,
    llm::response_types::ExecutionPlanContract,
    llm::{
        broker::{
            InputModality, ModelBroker, ModelSelectionRequest, OutputModality, ReasoningLevel,
        },
        ModelMetadata,
    },
    team::config::{TeamConfig, TeamRuntimeSettings},
};

use super::{
    dag::topological_levels,
    plan::{ExecutionPlan, PlanTask, TaskState},
};

const INTEGRATION_TASK_SLUG: &str = "task-integrate";

#[derive(Debug, Clone)]
struct PlanSeed {
    id: Option<String>,
    title: String,
    description: String,
    dependencies: Vec<String>,
    acceptance_criteria: Vec<String>,
    candidate_role: Option<String>,
    requested_model_route: Option<String>,
    expected_artifact: String,
    estimated_cost_usd: f64,
    estimated_ms: u64,
}

pub fn build_initial_plan(
    conversation_id: i64,
    goal: &str,
    team: &TeamConfig,
    roles: &[String],
    identity: &SystemIdentity,
    runtime_settings: &TeamRuntimeSettings,
    model_catalog: &[ModelMetadata],
) -> ExecutionPlan {
    let work_cap = team.plan_max_tasks.saturating_sub(1).max(1);
    let seeds = build_comparison_seeds(goal, work_cap)
        .or_else(|| build_structured_analysis_seeds(goal, work_cap))
        .unwrap_or_else(|| {
            split_goal(goal, work_cap)
                .into_iter()
                .enumerate()
                .map(|(idx, chunk)| PlanSeed {
                    id: Some(format!("task-{}", idx + 1)),
                    title: format!("Work package {}", idx + 1),
                    description: chunk,
                    dependencies: Vec::new(),
                    acceptance_criteria: vec![
                        "Answer the exact subproblem".to_string(),
                        "Provide concise evidence or rationale".to_string(),
                    ],
                    candidate_role: roles.get(idx % roles.len()).cloned(),
                    requested_model_route: None,
                    expected_artifact: "structured_markdown".to_string(),
                    estimated_cost_usd: 0.01,
                    estimated_ms: 1_500,
                })
                .collect::<Vec<_>>()
        });

    materialize_plan(
        conversation_id,
        goal,
        team,
        roles,
        identity,
        runtime_settings,
        model_catalog,
        vec!["User request can be decomposed safely".to_string()],
        vec![
            "Insufficient context may require clarification".to_string(),
            "External dependencies may fail".to_string(),
        ],
        seeds,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_plan_from_contract(
    conversation_id: i64,
    goal: &str,
    contract: ExecutionPlanContract,
    team: &TeamConfig,
    roles: &[String],
    identity: &SystemIdentity,
    runtime_settings: &TeamRuntimeSettings,
    model_catalog: &[ModelMetadata],
) -> ExecutionPlan {
    let ExecutionPlanContract {
        goal: contract_goal,
        assumptions,
        risks,
        tasks,
        parallelizable_groups: _,
    } = contract;
    let work_cap = team.plan_max_tasks.saturating_sub(1).max(1);
    let seeds = tasks
        .into_iter()
        .filter(|task| !task.description.trim().is_empty())
        .filter(|task| !is_integration_task_id(&task.id))
        .take(work_cap)
        .map(|task| PlanSeed {
            id: Some(task.id),
            title: task.title,
            description: task.description,
            dependencies: task.dependencies,
            acceptance_criteria: task.acceptance_criteria,
            candidate_role: task.candidate_role,
            requested_model_route: task.model_route,
            expected_artifact: task.expected_artifact,
            estimated_cost_usd: task.estimated_cost_usd,
            estimated_ms: task.estimated_ms,
        })
        .collect::<Vec<_>>();

    if seeds.is_empty() {
        return build_initial_plan(
            conversation_id,
            goal,
            team,
            roles,
            identity,
            runtime_settings,
            model_catalog,
        );
    }

    materialize_plan(
        conversation_id,
        if contract_goal.trim().is_empty() {
            goal
        } else {
            &contract_goal
        },
        team,
        roles,
        identity,
        runtime_settings,
        model_catalog,
        if assumptions.is_empty() {
            vec!["Planner omitted assumptions; using runtime defaults".to_string()]
        } else {
            assumptions
        },
        if risks.is_empty() {
            vec!["Planner omitted explicit risks".to_string()]
        } else {
            risks
        },
        seeds,
    )
}

pub fn plan_is_parallel_worth(plan: &ExecutionPlan) -> bool {
    let work_tasks = plan
        .tasks
        .iter()
        .filter(|task| !is_integration_task_id(&task.id))
        .count();
    work_tasks > 1
        || plan
            .parallelizable_groups
            .iter()
            .any(|group| group.len() > 1)
}

pub fn resolve_task_route_key(
    _identity: &SystemIdentity,
    runtime_settings: &TeamRuntimeSettings,
    candidate_role: Option<&str>,
    title: &str,
    description: &str,
    requested_model_route: Option<&str>,
) -> String {
    if let Some(explicit) = role_model_override(runtime_settings, candidate_role) {
        return normalize_route_key(&explicit)
            .unwrap_or_else(|| infer_route_key(candidate_role, title, description));
    }

    if let Some(requested) = requested_model_route {
        if let Some(route_key) = normalize_route_key(requested) {
            return route_key;
        }
    }

    infer_route_key(candidate_role, title, description)
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_task_model_selection(
    identity: &SystemIdentity,
    runtime_settings: &TeamRuntimeSettings,
    candidate_role: Option<&str>,
    title: &str,
    description: &str,
    requested_model_route: Option<&str>,
    model_catalog: &[ModelMetadata],
    estimated_cost_usd: f64,
    estimated_ms: u64,
) -> (Option<String>, String, String, u64) {
    let selected_route_key = resolve_task_route_key(
        identity,
        runtime_settings,
        candidate_role,
        title,
        description,
        requested_model_route,
    );
    let max_latency_ms = estimated_ms
        .max(250)
        .min(identity.frontmatter.budgets.timeout_ms.max(250));

    let selection = ModelBroker::new(model_catalog.to_vec()).resolve(
        &identity.frontmatter.model_routes,
        ModelSelectionRequest {
            route_key: &selected_route_key,
            input_modality: infer_input_modality(title, description),
            output_modality: infer_output_modality(title, description),
            reasoning_level: infer_reasoning_level(candidate_role, title, description),
            requires_tools: selected_route_key == "tool_use",
            max_cost_usd: Some(estimated_cost_usd.max(0.0)),
            max_latency_ms: Some(max_latency_ms),
            performance_policy: runtime_settings.performance_policy.clone(),
            escalation_tier: runtime_settings.max_escalation_tier.clone(),
        },
    );

    (
        Some(selection.route_key.clone()),
        selection.route_key,
        selection.resolved_model,
        max_latency_ms,
    )
}

fn infer_route_key(candidate_role: Option<&str>, title: &str, description: &str) -> String {
    let text = format!("{title}\n{description}").to_lowercase();
    if contains_any(
        &text,
        &[
            "predict",
            "forecast",
            "btc",
            "eth",
            "crypto",
            "mercado",
            "trading",
            "macro",
            "scenario",
            "escenario",
            "tesis",
            "thesis",
            "valuation",
            "cost-benefit",
            "costo beneficio",
            "compare deeply",
        ],
    ) {
        return "reasoning".to_string();
    }

    if candidate_role
        .map(|role| matches_role(role, &["verifier", "reviewer", "qa", "quality"]))
        .unwrap_or(false)
        || contains_any(
            &text,
            &["verify", "review", "check", "validate", "qa", "test"],
        )
    {
        return "reviewer_fast".to_string();
    }

    if candidate_role
        .map(|role| matches_role(role, &["integrator", "synthesizer"]))
        .unwrap_or(false)
        || contains_any(&text, &["integrate", "synthesize", "merge", "final answer"])
    {
        return "reasoning".to_string();
    }

    if contains_any(
        &text,
        &[
            "image",
            "photo",
            "picture",
            "screenshot",
            "vision",
            "ocr",
            "diagram",
        ],
    ) {
        return "vision_understand".to_string();
    }

    if candidate_role
        .map(|role| matches_role(role, &["implementer", "operator"]))
        .unwrap_or(false)
        || contains_any(
            &text,
            &[
                "http",
                "fetch",
                "api",
                "search",
                "buscar",
                "memory",
                "recordatorio",
                "reminder",
                "tool",
            ],
        )
    {
        return "tool_use".to_string();
    }

    "fast_text".to_string()
}

#[allow(clippy::too_many_arguments)]
fn materialize_plan(
    conversation_id: i64,
    goal: &str,
    team: &TeamConfig,
    roles: &[String],
    identity: &SystemIdentity,
    runtime_settings: &TeamRuntimeSettings,
    model_catalog: &[ModelMetadata],
    assumptions: Vec<String>,
    risks: Vec<String>,
    seeds: Vec<PlanSeed>,
) -> ExecutionPlan {
    let mut plan = ExecutionPlan::new(
        conversation_id,
        goal.to_string(),
        team.plan_max_depth as u32,
    );
    let work_cap = team.plan_max_tasks.saturating_sub(1).max(1);
    let usable_roles = if roles.is_empty() {
        runtime_settings.subagent_roleset.clone()
    } else {
        roles.to_vec()
    };

    let requested_ids = seeds
        .iter()
        .enumerate()
        .map(|(idx, seed)| {
            let local_id = sanitize_task_id(seed.id.as_deref(), idx)
                .unwrap_or_else(|| format!("task-{}", idx + 1));
            (idx, scoped_task_id(&plan.id, &local_id))
        })
        .collect::<HashMap<_, _>>();
    let valid_ids = requested_ids.values().cloned().collect::<HashSet<_>>();

    for (idx, seed) in seeds.into_iter().take(work_cap).enumerate() {
        let task_id = requested_ids
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| format!("task-{}", idx + 1));
        let candidate_role = resolve_candidate_role(
            &usable_roles,
            seed.candidate_role.as_deref(),
            &seed.title,
            &seed.description,
            idx,
        );
        let estimated_ms = seed.estimated_ms.max(250);
        let (model_route, route_key, resolved_model, max_latency_ms) = resolve_task_model_selection(
            identity,
            runtime_settings,
            candidate_role.as_deref(),
            &seed.title,
            &seed.description,
            seed.requested_model_route.as_deref(),
            model_catalog,
            seed.estimated_cost_usd.max(0.0),
            estimated_ms,
        );
        let dependencies = seed
            .dependencies
            .into_iter()
            .filter_map(|dep| sanitize_dependency_id(&dep))
            .map(|dep| scoped_task_id(&plan.id, &dep))
            .filter(|dep| dep != &task_id && valid_ids.contains(dep))
            .collect::<Vec<_>>();
        let acceptance_criteria = if seed.acceptance_criteria.is_empty() {
            vec![
                "Answer the assigned subproblem".to_string(),
                "Keep output concise and evidence-based".to_string(),
            ]
        } else {
            seed.acceptance_criteria
        };
        let requires_live_data = task_requires_live_data(goal, &seed.description);
        let evidence_inputs = infer_evidence_inputs(goal, &seed.description);
        let analysis_track = infer_analysis_track(&seed.title, &seed.description);

        plan.tasks.push(PlanTask {
            id: task_id,
            title: if seed.title.trim().is_empty() {
                format!("Work package {}", idx + 1)
            } else {
                seed.title
            },
            description: seed.description,
            depth: 1,
            dependencies,
            acceptance_criteria,
            candidate_role,
            model_route,
            route_key,
            resolved_model,
            requires_live_data,
            evidence_inputs,
            analysis_track,
            expected_artifact: if seed.expected_artifact.trim().is_empty() {
                "structured_markdown".to_string()
            } else {
                seed.expected_artifact
            },
            estimated_cost_usd: seed.estimated_cost_usd.max(0.0),
            estimated_ms,
            max_latency_ms,
            state: TaskState::Ready,
            attempts: 0,
            review_loops: 0,
        });
    }

    let integration_deps = plan
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let integration_role = resolve_candidate_role(
        &usable_roles,
        Some("integrator"),
        "Integrate accepted artifacts",
        "Merge accepted outputs into final answer",
        plan.tasks.len(),
    );
    let integration_route_preference = if requires_strict_reasoning(goal) {
        "integrator_complex"
    } else {
        "fast_text"
    };
    let (
        integration_model_route,
        integration_route_key,
        integration_resolved_model,
        integration_max_latency_ms,
    ) = resolve_task_model_selection(
        identity,
        runtime_settings,
        integration_role.as_deref(),
        "Integrate accepted artifacts",
        "Merge accepted outputs into final answer",
        Some(integration_route_preference),
        model_catalog,
        0.01,
        1_200,
    );
    plan.tasks.push(PlanTask {
        id: scoped_task_id(&plan.id, INTEGRATION_TASK_SLUG),
        title: "Integrate accepted artifacts".to_string(),
        description: "Merge accepted outputs into final answer".to_string(),
        depth: 1,
        dependencies: integration_deps,
        acceptance_criteria: vec![
            "All mandatory artifacts are represented".to_string(),
            "Final output is coherent and safe".to_string(),
        ],
        candidate_role: integration_role.clone(),
        model_route: integration_model_route,
        route_key: integration_route_key,
        resolved_model: integration_resolved_model,
        requires_live_data: task_requires_live_data(
            goal,
            "Merge accepted outputs into final answer",
        ),
        evidence_inputs: infer_evidence_inputs(goal, "Merge accepted outputs into final answer"),
        analysis_track: "integration".to_string(),
        expected_artifact: "final_answer_draft".to_string(),
        estimated_cost_usd: 0.01,
        estimated_ms: 1_200,
        max_latency_ms: integration_max_latency_ms,
        state: TaskState::Pending,
        attempts: 0,
        review_loops: 0,
    });

    let depth_by_task = topological_levels(&plan.tasks)
        .into_iter()
        .enumerate()
        .flat_map(|(level, task_ids)| {
            task_ids
                .into_iter()
                .map(move |task_id| (task_id, (level + 1) as u32))
        })
        .collect::<HashMap<_, _>>();
    for task in &mut plan.tasks {
        task.depth = depth_by_task
            .get(&task.id)
            .copied()
            .unwrap_or(1)
            .min(team.plan_max_depth as u32);
        task.state = if task.dependencies.is_empty() {
            TaskState::Ready
        } else {
            TaskState::Pending
        };
    }

    plan.parallelizable_groups = topological_levels(&plan.tasks);
    plan.assumptions = assumptions;
    plan.risks = risks;
    plan
}

fn split_goal(goal: &str, max_tasks: usize) -> Vec<String> {
    let normalized = goal
        .replace('\n', ".")
        .replace(" and ", ".")
        .replace(" y ", ".")
        .replace(" luego ", ".")
        .replace(" despues ", ".")
        .replace([';', ','], ".");

    let mut chunks = normalized
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    if chunks.is_empty() {
        chunks.push(goal.to_string());
    }
    chunks.truncate(max_tasks.max(1));
    chunks
}

fn build_comparison_seeds(goal: &str, max_tasks: usize) -> Option<Vec<PlanSeed>> {
    let normalized = goal.to_lowercase();
    let comparison_marker = normalized.contains("compara")
        || normalized.contains("compare")
        || normalized.contains("comparación")
        || normalized.contains("comparacion")
        || normalized.contains("ranking");
    if !comparison_marker {
        return None;
    }

    let criteria = [
        (
            "motor",
            "Analiza motor, respuesta y agrado de manejo de cada opcion.",
        ),
        (
            "consumo",
            "Compara consumo, autonomia y costo operativo de cada opcion.",
        ),
        (
            "reventa",
            "Evalua liquidez de reventa y retencion de valor de cada opcion.",
        ),
        (
            "repuestos",
            "Compara disponibilidad y costo de repuestos y mantenimiento.",
        ),
        (
            "confiabilidad",
            "Evalua confiabilidad historica y riesgos mecanicos relevantes.",
        ),
        (
            "diversion",
            "Compara sensacion de manejo, respuesta y diversion al volante.",
        ),
        (
            "costo",
            "Compara costo total de propiedad y costo-beneficio de cada opcion.",
        ),
        (
            "riesgo",
            "Evalua riesgos clave, tradeoffs y posibles puntos debiles de cada opcion.",
        ),
    ];

    let matched = criteria
        .iter()
        .filter(|(needle, _)| normalized.contains(*needle))
        .map(|(needle, description)| (*needle, *description))
        .collect::<Vec<_>>();

    let grouped_tracks = if matched.len() >= 5 {
        vec![
            (
                "economia",
                vec!["consumo", "costo", "repuestos"],
                "Compara consumo, costo total de propiedad y mantenimiento/repuestos de cada opcion.",
                "researcher",
                "fast_text",
            ),
            (
                "riesgo_valor",
                vec!["reventa", "confiabilidad", "riesgo"],
                "Evalua reventa, confiabilidad y riesgos clave de cada opcion, explicando los tradeoffs principales.",
                "verifier",
                "reasoning",
            ),
            (
                "manejo",
                vec!["motor", "diversion"],
                "Compara motor, respuesta, agrado de manejo y diversion al volante de cada opcion.",
                "researcher",
                "fast_text",
            ),
        ]
        .into_iter()
        .filter(|(_, needles, _, _, _)| needles.iter().any(|needle| normalized.contains(needle)))
        .map(|(_, needles, description, role, route)| {
            let selected = needles
                .iter()
                .filter(|needle| normalized.contains(**needle))
                .copied()
                .collect::<Vec<_>>();
            (description.to_string(), role.to_string(), route.to_string(), selected)
        })
        .collect::<Vec<_>>()
    } else {
        matched
            .into_iter()
            .map(|(needle, description)| {
                (
                    format!(
                        "{} Usa el contexto de la conversacion actual y mantente enfocado en el criterio '{}'.",
                        description, needle
                    ),
                    if matches!(needle, "riesgo" | "confiabilidad") {
                        "verifier".to_string()
                    } else {
                        "researcher".to_string()
                    },
                    if matches!(needle, "riesgo" | "confiabilidad") {
                        "reviewer_fast".to_string()
                    } else {
                        "fast_text".to_string()
                    },
                    vec![needle],
                )
            })
            .collect::<Vec<_>>()
    };

    let mut seeds = grouped_tracks
        .into_iter()
        .take(max_tasks.max(1))
        .enumerate()
        .map(
            |(idx, (description, role, route, criteria_list))| PlanSeed {
                id: Some(format!("task-{}", idx + 1)),
                title: format!("Comparison track {}", idx + 1),
                description: format!(
                    "{} Criterios cubiertos: {}.",
                    description,
                    criteria_list.join(", ")
                ),
                dependencies: Vec::new(),
                acceptance_criteria: vec![
                    "Compare all named options for the assigned criterion set".to_string(),
                    "State tradeoffs clearly".to_string(),
                    format!("Cover these criteria: {}", criteria_list.join(", ")),
                ],
                candidate_role: Some(role),
                requested_model_route: Some(route),
                expected_artifact: if criteria_list.len() > 1 {
                    "comparison_bundle".to_string()
                } else {
                    "criterion_comparison".to_string()
                },
                estimated_cost_usd: 0.01,
                estimated_ms: if criteria_list.len() > 1 {
                    2_200
                } else {
                    1_700
                },
            },
        )
        .collect::<Vec<_>>();

    if seeds.is_empty() {
        return None;
    }

    let grouped_comparison = seeds
        .iter()
        .any(|seed| seed.expected_artifact == "comparison_bundle");

    if normalized.contains("ranking") && seeds.len() < max_tasks && !grouped_comparison {
        let ranking_idx = seeds.len() + 1;
        seeds.push(PlanSeed {
            id: Some(format!("task-{ranking_idx}")),
            title: "Build ranking recommendation".to_string(),
            description:
                "Construye un ranking provisional con los hallazgos disponibles y explicita el criterio de desempate."
                    .to_string(),
            dependencies: Vec::new(),
            acceptance_criteria: vec![
                "Provide a provisional ranking".to_string(),
                "Explain the rationale behind the ranking".to_string(),
            ],
            candidate_role: Some("verifier".to_string()),
            requested_model_route: Some("reasoning".to_string()),
            expected_artifact: "ranking_draft".to_string(),
            estimated_cost_usd: 0.015,
            estimated_ms: 3_200,
        });
    }

    Some(seeds)
}

fn build_structured_analysis_seeds(goal: &str, max_tasks: usize) -> Option<Vec<PlanSeed>> {
    let normalized = goal.to_lowercase();
    let looks_like_business_analysis = contains_any(
        &normalized,
        &[
            "saas",
            "startup",
            "lanzar",
            "launch",
            "go-to-market",
            "traders",
            "mercado",
            "pricing",
            "regulación",
            "regulacion",
            "costos operativos",
            "costs",
            "arquitectura",
            "plan de lanzamiento",
        ],
    );
    let has_list_shape = normalized.contains(';')
        || normalized.matches(',').count() >= 4
        || normalized.contains("separa en subtareas")
        || normalized.contains("divide entre muchos subagentes");
    if !looks_like_business_analysis || !has_list_shape {
        return None;
    }

    let grouped_tracks = vec![
        (
            "market_gtm",
            vec!["tam", "sam", "som", "competencia", "pricing", "adquisición de clientes", "adquisicion de clientes", "go-to-market", "gtm"],
            "Analiza mercado, TAM/SAM/SOM, competencia, pricing y adquisición de clientes. Termina con una propuesta comercial compacta.",
            "researcher",
            "reasoning",
            0.02,
            2_600,
        ),
        (
            "regulatory_risk",
            vec!["regulación", "regulacion", "riesgos legales", "riesgos operativos", "compliance", "legal"],
            "Evalua regulación, compliance y riesgos legales/operativos. Prioriza los riesgos que realmente pueden bloquear el lanzamiento.",
            "verifier",
            "reasoning",
            0.02,
            2_800,
        ),
        (
            "product_delivery",
            vec!["arquitectura técnica", "arquitectura", "costos operativos", "infraestructura", "mvp", "plan de lanzamiento", "90 días", "90 dias"],
            "Define arquitectura técnica, costos operativos y un plan de lanzamiento realista de 90 días con hitos y dependencias.",
            "implementer",
            "reasoning",
            0.02,
            2_800,
        ),
        (
            "executive_positioning",
            vec!["recomendación ejecutiva", "recomendacion ejecutiva", "tradeoffs", "tesis", "positioning"],
            "Prepara una tesis ejecutiva provisional con tradeoffs y condiciones para avanzar o no avanzar.",
            "integrator",
            "reviewer_strict",
            0.015,
            2_200,
        ),
    ];

    let mut seeds = grouped_tracks
        .into_iter()
        .filter_map(
            |(_slug, needles, description, role, route, estimated_cost_usd, estimated_ms)| {
                let selected = needles
                    .iter()
                    .filter(|needle| normalized.contains(**needle))
                    .copied()
                    .collect::<Vec<_>>();
                if selected.is_empty() {
                    return None;
                }
                Some((
                    description,
                    role,
                    route,
                    estimated_cost_usd,
                    estimated_ms,
                    selected,
                ))
            },
        )
        .take(max_tasks.max(1))
        .enumerate()
        .map(
            |(idx, (description, role, route, estimated_cost_usd, estimated_ms, selected))| {
                PlanSeed {
                    id: Some(format!("task-{}", idx + 1)),
                    title: format!("Analysis track {}", idx + 1),
                    description: format!(
                        "{} Ejes cubiertos: {}.",
                        description,
                        selected.join(", ")
                    ),
                    dependencies: Vec::new(),
                    acceptance_criteria: vec![
                        "Answer the assigned analysis track".to_string(),
                        "Call out the main tradeoffs and evidence".to_string(),
                        format!("Cover these axes: {}", selected.join(", ")),
                    ],
                    candidate_role: Some(role.to_string()),
                    requested_model_route: Some(route.to_string()),
                    expected_artifact: "analysis_bundle".to_string(),
                    estimated_cost_usd,
                    estimated_ms,
                }
            },
        )
        .collect::<Vec<_>>();

    if seeds.is_empty() {
        return None;
    }

    let long_list_hint =
        normalized.matches(',').count() >= 6 || normalized.matches(';').count() >= 3;
    if long_list_hint && seeds.len() < max_tasks && seeds.len() < 4 {
        let next_idx = seeds.len() + 1;
        seeds.push(PlanSeed {
            id: Some(format!("task-{next_idx}")),
            title: "Synthesize recommendation constraints".to_string(),
            description: "Resume los criterios de decisión, condiciones de entrada y señales de no-go para que la integración final tenga una base ejecutiva clara.".to_string(),
            dependencies: Vec::new(),
            acceptance_criteria: vec![
                "Produce explicit go/no-go gates".to_string(),
                "Summarize decision constraints concisely".to_string(),
            ],
            candidate_role: Some("verifier".to_string()),
            requested_model_route: Some("reasoning".to_string()),
            expected_artifact: "decision_constraints".to_string(),
            estimated_cost_usd: 0.015,
            estimated_ms: 2_000,
        });
    }

    Some(seeds)
}

fn scoped_task_id(plan_id: &str, local_id: &str) -> String {
    format!("{plan_id}:{local_id}")
}

fn is_integration_task_id(task_id: &str) -> bool {
    task_id
        .rsplit(':')
        .next()
        .is_some_and(|slug| slug == INTEGRATION_TASK_SLUG)
}

fn sanitize_task_id(raw: Option<&str>, index: usize) -> Option<String> {
    let cleaned = raw
        .unwrap_or_default()
        .trim()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        return Some(format!("task-{}", index + 1));
    }
    Some(cleaned)
}

fn sanitize_dependency_id(raw: &str) -> Option<String> {
    let cleaned = raw
        .trim()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase();
    (!cleaned.is_empty()).then_some(cleaned)
}

fn resolve_candidate_role(
    roles: &[String],
    requested: Option<&str>,
    title: &str,
    description: &str,
    idx: usize,
) -> Option<String> {
    if roles.is_empty() {
        return requested.map(ToString::to_string);
    }

    if let Some(role) = requested {
        if let Some(matched) = roles.iter().find(|item| item.eq_ignore_ascii_case(role)) {
            return Some(matched.clone());
        }
    }

    let text = format!("{title}\n{description}").to_lowercase();
    let hints: &[(&[&str], &str)] = &[
        (&["verify", "review", "check", "validate"], "verifier"),
        (&["integrate", "merge", "synthesize", "final"], "integrator"),
        (&["implement", "build", "execute", "produce"], "implementer"),
        (&["research", "search", "compare", "gather"], "researcher"),
    ];

    for &(needles, role) in hints {
        if contains_any(&text, needles) {
            if let Some(matched) = roles.iter().find(|item| item.eq_ignore_ascii_case(role)) {
                return Some(matched.clone());
            }
        }
    }

    roles.get(idx % roles.len()).cloned()
}

fn role_model_override(
    runtime_settings: &TeamRuntimeSettings,
    candidate_role: Option<&str>,
) -> Option<String> {
    let role = candidate_role?;
    runtime_settings
        .role_specializations
        .iter()
        .find_map(|(configured_role, spec)| {
            configured_role
                .eq_ignore_ascii_case(role)
                .then(|| spec.model_route.clone())
                .flatten()
        })
}

fn normalize_route_key(requested: &str) -> Option<String> {
    let normalized = requested.trim().to_ascii_lowercase();
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
        "reasoning_high" | "deep_reasoning" | "integrator_strong" => Some("reasoning".to_string()),
        _ => None,
    }
}

fn task_requires_live_data(goal: &str, description: &str) -> bool {
    let normalized = format!("{goal}\n{description}").to_lowercase();
    contains_any(
        &normalized,
        &[
            "al día de hoy",
            "al dia de hoy",
            "precio actual",
            "current price",
            "hoy",
            "latest",
            "mercado",
            "trading",
            "noticia",
            "noticias",
            "news",
            "btc",
            "bitcoin",
            "ethereum",
            "solana",
            "url",
            "link",
            "enlace",
        ],
    )
}

fn infer_evidence_inputs(goal: &str, description: &str) -> Vec<String> {
    let normalized = format!("{goal}\n{description}").to_lowercase();
    let mut inputs = Vec::new();
    for (needle, value) in [
        ("bitcoin", "bitcoin"),
        ("btc", "bitcoin"),
        ("ethereum", "ethereum"),
        ("eth", "ethereum"),
        ("solana", "solana"),
        ("sol", "solana"),
        ("noticias", "news"),
        ("news", "news"),
    ] {
        if normalized.contains(needle) {
            inputs.push(value.to_string());
        }
    }
    inputs.sort();
    inputs.dedup();
    inputs
}

fn infer_analysis_track(title: &str, description: &str) -> String {
    let normalized = format!("{title}\n{description}").to_lowercase();
    if contains_any(
        &normalized,
        &["integrate", "merge", "synthesize", "final answer"],
    ) {
        return "integration".to_string();
    }
    if contains_any(
        &normalized,
        &["verify", "review", "validate", "risk", "compliance"],
    ) {
        return "validation".to_string();
    }
    if contains_any(
        &normalized,
        &["market", "pricing", "tam", "sam", "som", "competencia"],
    ) {
        return "market_research".to_string();
    }
    if contains_any(
        &normalized,
        &["motor", "consumo", "reventa", "repuestos", "diversion"],
    ) {
        return "comparison".to_string();
    }
    if contains_any(
        &normalized,
        &["teorema", "algoritmo", "moral", "computabilidad"],
    ) {
        return "theory".to_string();
    }
    "general_analysis".to_string()
}

fn requires_strict_reasoning(goal: &str) -> bool {
    let normalized = goal.to_lowercase();
    contains_any(
        &normalized,
        &[
            "btc",
            "bitcoin",
            "eth",
            "ethereum",
            "solana",
            "forecast",
            "predic",
            "trading",
            "mercado",
            "market",
            "macro",
            "probabilidad",
            "scenario",
            "escenario",
            "thesis",
            "tesis",
            "riesgo sist",
            "al dia de hoy",
            "al día de hoy",
            "ranking",
            "compara",
            "comparacion",
            "comparación",
            "recomend",
        ],
    )
}

fn infer_input_modality(title: &str, description: &str) -> InputModality {
    let text = format!("{title}\n{description}").to_lowercase();
    if contains_any(
        &text,
        &["audio", "voice", "speech", "transcribe", "recording"],
    ) {
        InputModality::Audio
    } else if contains_any(
        &text,
        &[
            "image",
            "photo",
            "picture",
            "screenshot",
            "vision",
            "ocr",
            "diagram",
        ],
    ) {
        InputModality::Image
    } else {
        InputModality::Text
    }
}

fn infer_output_modality(title: &str, description: &str) -> OutputModality {
    let text = format!("{title}\n{description}").to_lowercase();
    if contains_any(&text, &["generate image", "render image", "image output"]) {
        OutputModality::Image
    } else if contains_any(&text, &["strict json", "json", "schema"]) {
        OutputModality::Json
    } else {
        OutputModality::Text
    }
}

fn infer_reasoning_level(
    candidate_role: Option<&str>,
    title: &str,
    description: &str,
) -> ReasoningLevel {
    let text = format!("{title}\n{description}").to_lowercase();
    if candidate_role
        .map(|role| matches_role(role, &["integrator", "synthesizer"]))
        .unwrap_or(false)
        || contains_any(
            &text,
            &[
                "integrate",
                "synthesize",
                "merge",
                "plan",
                "final answer",
                "predict",
                "forecast",
                "trading",
                "btc",
                "crypto",
                "investment",
                "portafolio",
                "scenario",
                "estrategia",
            ],
        )
    {
        ReasoningLevel::High
    } else if candidate_role
        .map(|role| matches_role(role, &["verifier", "reviewer", "qa", "quality"]))
        .unwrap_or(false)
        || contains_any(
            &text,
            &[
                "verify", "review", "check", "validate", "compare", "analiza", "analyze",
                "contrast",
            ],
        )
    {
        ReasoningLevel::Medium
    } else {
        ReasoningLevel::Low
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn matches_role(role: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|alias| role.eq_ignore_ascii_case(alias))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{
        identity::{
            compiler::{CompiledIdentitySections, SystemIdentity},
            schema::{
                IdentityBudgets, IdentityChannels, IdentityFrontmatter, IdentityMemory,
                IdentityPermissions, ModelRoutes, TelegramIdentityChannel,
            },
        },
        llm::response_types::{ExecutionPlanContract, LlmPlanTask},
        team::config::{
            PrincipalSkillConfig, RoleSpecialization, SubagentMode, TeamConfig, TeamRuntimeSettings,
        },
    };

    use super::{build_initial_plan, build_plan_from_contract, plan_is_parallel_worth};

    fn identity() -> SystemIdentity {
        SystemIdentity {
            frontmatter: IdentityFrontmatter {
                id: "ferrum".to_string(),
                display_name: "Ferrum".to_string(),
                description: "test".to_string(),
                locale: "en-US".to_string(),
                timezone: "UTC".to_string(),
                model_routes: ModelRoutes {
                    fast: "model-fast".to_string(),
                    reasoning: "model-reason".to_string(),
                    tool_use: "model-tool".to_string(),
                    vision: "model-vision".to_string(),
                    reviewer: "model-review".to_string(),
                    planner: "model-plan".to_string(),
                    router_fast: None,
                    fast_text: None,
                    reviewer_fast: None,
                    reviewer_strict: None,
                    integrator_complex: None,
                    vision_understand: None,
                    audio_transcribe: None,
                    image_generate: None,
                    fallback: vec![],
                },
                budgets: IdentityBudgets {
                    max_steps: 8,
                    max_turn_cost_usd: 1.0,
                    max_input_tokens: 4000,
                    max_output_tokens: 800,
                    max_tool_calls: 4,
                    timeout_ms: 10_000,
                },
                memory: IdentityMemory {
                    save_facts: true,
                    save_summaries: true,
                    summarize_every_n_turns: 4,
                },
                permissions: IdentityPermissions {
                    allowed_skills: vec!["*".to_string()],
                    denied_skills: vec![],
                },
                channels: IdentityChannels {
                    telegram: TelegramIdentityChannel {
                        enabled: true,
                        max_reply_chars: 3000,
                        style_overrides: "concise".to_string(),
                    },
                },
            },
            sections: CompiledIdentitySections {
                mission: "mission".to_string(),
                persona: "persona".to_string(),
                tone: "tone".to_string(),
                hard_rules: "rules".to_string(),
                do_not_do: "dont".to_string(),
                escalation: "esc".to_string(),
                memory_preferences: "mem".to_string(),
                channel_notes: "notes".to_string(),
                planning_principles: "plan".to_string(),
                review_standards: "review".to_string(),
            },
            compiled_system_prompt: "system".to_string(),
        }
    }

    fn team_config() -> TeamConfig {
        TeamConfig {
            team_size: 4,
            max_parallel_tasks: 4,
            allow_ephemeral_subagents: true,
            max_ephemeral_subagents: 6,
            subagent_mode: SubagentMode::Generalist,
            subagent_roleset: vec![
                "researcher".to_string(),
                "implementer".to_string(),
                "verifier".to_string(),
                "integrator".to_string(),
            ],
            subagent_profile_path: None,
            supervisor_review_interval_ms: 1000,
            max_review_loops_per_task: 3,
            max_task_retries: 2,
            plan_max_tasks: 8,
            plan_max_depth: 3,
            performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: crate::team::config::EscalationTier::Standard,
            typing_delay_ms: 800,
            require_final_review: true,
            progress_updates_enabled: true,
            progress_update_threshold_ms: 5000,
        }
    }

    fn runtime_settings() -> TeamRuntimeSettings {
        TeamRuntimeSettings {
            team_size: 4,
            max_parallel_tasks: 4,
            allow_ephemeral_subagents: true,
            max_ephemeral_subagents: 6,
            subagent_mode: SubagentMode::Generalist,
            subagent_roleset: vec![
                "researcher".to_string(),
                "implementer".to_string(),
                "verifier".to_string(),
                "integrator".to_string(),
            ],
            subagent_profile_path: None,
            supervisor_review_interval_ms: 1000,
            max_review_loops_per_task: 3,
            max_task_retries: 2,
            plan_max_tasks: 8,
            plan_max_depth: 3,
            performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: crate::team::config::EscalationTier::Standard,
            typing_delay_ms: 800,
            require_final_review: true,
            progress_updates_enabled: true,
            progress_update_threshold_ms: 5000,
            principal_skills: PrincipalSkillConfig {
                allowed_skills: vec!["*".to_string()],
                denied_skills: vec![],
            },
            role_specializations: HashMap::from([(
                "verifier".to_string(),
                RoleSpecialization {
                    allowed_skills: vec![],
                    model_route: Some("reviewer_strict".to_string()),
                },
            )]),
            subagent_specializations: HashMap::new(),
        }
    }

    #[test]
    fn fallback_plan_assigns_models() {
        let plan = build_initial_plan(
            1,
            "Research pricing and verify the result",
            &team_config(),
            &runtime_settings().subagent_roleset,
            &identity(),
            &runtime_settings(),
            &[],
        );

        assert!(plan.tasks.iter().any(|task| task.model_route.is_some()));
        assert!(plan_is_parallel_worth(&plan));
    }

    #[test]
    fn contract_plan_resolves_route_names_and_roles() {
        let contract = ExecutionPlanContract {
            goal: "Need a plan".to_string(),
            assumptions: vec!["A".to_string()],
            risks: vec!["R".to_string()],
            tasks: vec![
                LlmPlanTask {
                    id: "collect".to_string(),
                    title: "Collect evidence".to_string(),
                    description: "Research the issue".to_string(),
                    dependencies: vec![],
                    acceptance_criteria: vec!["Useful".to_string()],
                    candidate_role: Some("researcher".to_string()),
                    model_route: Some("fast".to_string()),
                    requires_live_data: false,
                    evidence_inputs: vec![],
                    analysis_track: "research".to_string(),
                    expected_artifact: "notes".to_string(),
                    estimated_cost_usd: 0.01,
                    estimated_ms: 800,
                },
                LlmPlanTask {
                    id: "review".to_string(),
                    title: "Review evidence".to_string(),
                    description: "Verify conclusions".to_string(),
                    dependencies: vec!["collect".to_string()],
                    acceptance_criteria: vec!["Verified".to_string()],
                    candidate_role: Some("verifier".to_string()),
                    model_route: None,
                    requires_live_data: false,
                    evidence_inputs: vec![],
                    analysis_track: "validation".to_string(),
                    expected_artifact: "review".to_string(),
                    estimated_cost_usd: 0.01,
                    estimated_ms: 800,
                },
            ],
            parallelizable_groups: vec![],
        };

        let plan = build_plan_from_contract(
            2,
            "Need a plan",
            contract,
            &team_config(),
            &runtime_settings().subagent_roleset,
            &identity(),
            &runtime_settings(),
            &[],
        );

        assert_eq!(plan.tasks[0].model_route.as_deref(), Some("router_fast"));
        assert_eq!(plan.tasks[0].resolved_model, "model-fast");
        assert_eq!(
            plan.tasks[1].model_route.as_deref(),
            Some("reviewer_strict")
        );
        assert_eq!(plan.tasks[1].resolved_model, "model-reason");
        assert!(plan
            .tasks
            .iter()
            .any(|task| task.id.ends_with(":task-integrate")));
    }

    #[test]
    fn comparison_plan_creates_parallel_tracks() {
        let plan = build_initial_plan(
            3,
            "Compara Toyota Corolla, Honda Civic y Hyundai Elantra por motor, consumo, reventa, repuestos, confiabilidad, diversion, costo total y riesgo, y dame un ranking final.",
            &team_config(),
            &runtime_settings().subagent_roleset,
            &identity(),
            &runtime_settings(),
            &[],
        );

        let work_tasks = plan
            .tasks
            .iter()
            .filter(|task| !task.id.ends_with(":task-integrate"))
            .count();
        assert!(work_tasks >= 3);
        assert!(plan
            .parallelizable_groups
            .iter()
            .any(|group| group.len() >= 3));
    }
}
