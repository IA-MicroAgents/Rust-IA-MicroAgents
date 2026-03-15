# Ferrum Master Plan

## Purpose
Ferrum exists to run a serious AI operator with deterministic control, bounded costs, and operator-grade introspection. It is optimized for one strong engineer operating one binary. The active channel is Telegram Bot API only.

## Product Contract
Ferrum must:
1. Ingest Telegram updates and persist raw payloads immediately.
2. Process channel events asynchronously with bounded queues.
3. Build bounded execution plans (DAG) for complex requests.
4. Dispatch independent tasks in parallel to bounded subagents.
5. Continuously review outputs and enforce retries/reassignments within limits.
6. Integrate accepted artifacts and send only final supervisor-approved answer.
7. Persist full trace (plan/task/review/artifacts/model/tool/outbound).
8. Expose live dashboard and SSE stream.

## Design Principles
- Supervisor-led team, not autonomous swarm behavior.
- One orchestrator state machine with explicit transitions and budgets.
- Deterministic tool execution with JSON Schema validation.
- Fail closed on invalid identity/skills/permissions/schema.
- Human-editable markdown (`IDENTITY.md`, `skills/*/SKILL.md`) as source of truth.
- Postgres as primary store and Redis as hot cache.

## Runtime Flow
1. `inbound_webhook_received`
2. persist inbound event
3. dedupe
4. normalize conversation turn
5. load identity + memory + relevant skills
6. route decision (`direct_reply`, `tool_use`, `plan_then_act`, `ignore`, `ask_clarification`)
7. for `plan_then_act`: build plan DAG + persist + emit events
8. run ready tasks in parallel
9. review each artifact, bound loops/retries
10. integrate accepted artifacts
11. final delivery via active channel
12. persist usage/cost/events/traces

## Bounded Controls
- max team size
- max ephemeral subagents
- max parallel tasks
- max plan tasks/depth
- max review loops
- max retries
- max turn steps/cost/tokens/timeouts
- outbound kill switch

## Current v1 Scope
- Telegram Bot API long polling (text-first)
- OpenRouter provider (chat completions + model validation)
- Dashboard + API + SSE from Rust
- Builtin/command/http skills
- Durable reminders scheduler
- Postgres primary persistence with cached read paths

## Out of Scope (v1)
- Independent multi-service agent mesh
- Mandatory embeddings/vector DB
- Heavy frontend SPA toolchain

## Done Criteria
- Identity reload alters behavior at runtime.
- Skill folder changes reload live.
- Team size env var controls worker slots.
- Complex request yields real plan DAG and parallel task execution.
- Reviewed artifacts are integrated by supervisor.
- Dashboard shows live state/events.
- Startup fails fast on invalid model routes.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` all pass.
