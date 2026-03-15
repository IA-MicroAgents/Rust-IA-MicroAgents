# AI MicroAgents Architecture

## High-Level Components
- `src/app.rs`: runtime assembly, queue workers, CLI actions.
- `src/http/`: webhook, health, metrics.
- `src/dashboard/`: SSR dashboard routes + operator API + SSE.
- `src/orchestrator/`: supervisor flow and turn lifecycle.
- `src/planner/`: plan DAG data model and decomposition.
- `src/execution/`: parallel scheduler/dispatcher/task artifacts.
- `src/team/`: subagent config, manager, worker, reviewer, heartbeats.
- `src/skills/`: manifest parsing, registry, selector, safe runner.
- `src/identity/`: YAML+markdown identity parser/compiler/hot reload.
- `src/llm/`: provider trait and OpenRouter implementation.
- `src/storage/`: storage abstraction, Postgres persistence, Redis cache layer.
- `src/scheduler/`: durable reminders/delayed jobs.
- `src/telemetry/`: logging, metrics, trace IDs, runtime event bus.

## State Machines
### Runtime
- `idle -> receiving_event -> normalizing -> planning -> dispatching -> executing -> reviewing -> integrating -> delivering -> completed|failed`
- `paused` can be entered/exited by operator action.

### Task Lifecycle
- `pending -> ready -> assigned -> running -> waiting_review -> accepted|rejected -> retrying -> completed|failed|cancelled`

## Plan DAG
`ExecutionPlan` is persisted with:
- goal
- assumptions/risks
- tasks
- dependencies
- parallelizable groups
- acceptance criteria
- role candidates

`planner::dag::ready_task_ids` drives dependency-safe dispatch.

## Parallel Execution Model
- Tokio `Semaphore` bounds concurrent task execution.
- Team manager exposes bounded subagent slots.
- Dispatcher acquires subagent, runs worker, runs review, releases slot.
- Scheduler updates task states and retries within configured limits.

## Review Loop
1. subagent submits artifact
2. deterministic acceptance score
3. reviewer model decision (if needed)
4. supervisor action (`accept`, `request_revision`, `retry`, etc.)
5. state + event persistence

## Persistence (Postgres + Redis)
Core tables:
- `inbound_events`, `processed_event_dedup`
- `conversations`, `turns`, `summaries`, `facts`
- `plans`, `tasks`, `task_attempts`, `task_artifacts`, `task_reviews`
- `subagent_states`, `subagent_heartbeats`
- `runtime_events`
- `tool_traces`, `model_usages`, `outbound_messages`
- `jobs`, `reminders`

Cache keys:
- `dashboard:*` for recent runtime events, plan/task snapshots, total cost
- `conversation:*` for recent turns and latest summary
- `memory:*` for retrieval result caching

Persistence model:
- Postgres is the source of truth.
- Redis is an opportunistic cache; losing it does not lose data.

## Event Bus
Typed runtime events are broadcast in-process and persisted (`runtime_events`) for replay and dashboard introspection.

## Dashboard
Server-rendered Askama views + JSON APIs + SSE stream.
No JS build chain. Minimal static assets in `static/`.

## Safety Boundaries
- permissions allow/deny checks from identity
- per-skill schemas and timeouts
- HTTP domain allowlist
- outbound kill switch
- redaction in logs/dashboard-facing config views
