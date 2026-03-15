# ferrum

`ferrum` is a production-grade Rust runtime for AI orchestration with OpenRouter, bounded supervisor-led task execution, and a live operator dashboard. The operator path is Telegram Bot API only.

## Highlights
- Supervisor-led orchestration with bounded worker subagents.
- Planning as DAG (`plan -> tasks -> dependencies -> review -> integration`).
- Parallel task execution with Tokio semaphores.
- Identity as code via `IDENTITY.md` (hot reload, last-known-good fallback).
- Skills as code via `skills/<skill>/SKILL.md` (hot reload, schema/permission enforcement).
- Telegram Bot API support with official long polling and outbound `sendMessage`.
- Postgres as primary durable store for events, turns, plans, tasks, artifacts, reviews, reminders.
- Redis as hot cache for dashboard reads, plan/task snapshots, recent turns, summaries, and memory retrieval.
- Real-time dashboard + SSE event stream from Rust (no Node build).
- Safety controls: per-turn/per-task budgets, kill switch, retries, timeout, redaction.

## Quickstart
1. `cp .env.example .env`
2. Start Postgres and Redis locally or point the env vars at existing instances.
3. Fill `.env` with:
   - `OPENROUTER_API_KEY`
   - `TELEGRAM_BOT_TOKEN`
   - optional `TELEGRAM_BOT_USERNAME`
4. Create a Telegram bot with `@BotFather` and copy the token into `TELEGRAM_BOT_TOKEN`.
5. Validate configuration:
   - `cargo run -- doctor`
6. Run runtime:
   - `cargo run -- run`
7. Open dashboard:
   - `http://localhost:8080/dashboard`
8. Configure team size, parallelism, specializations, and ephemeral workers from the dashboard `Config` tab.

If `FERRUM_DASHBOARD_AUTH_TOKEN` is set, send header `x-ferrum-dashboard-token`.

## Minimal Environment
`/.env.example` contains the current minimum runtime configuration:
- bind/runtime basics
- Postgres
- Redis
- OpenRouter
- Telegram
- safety controls
- dashboard

Team topology and specialization are now primarily runtime-managed from the dashboard, not from `.env`.

## Telegram Setup
- Create the bot via `@BotFather`.
- Set:
  - `TELEGRAM_ENABLED=true`
  - `TELEGRAM_BOT_TOKEN=...`
  - `TELEGRAM_BOT_USERNAME=your_bot_username` (optional but useful for dashboard deep-linking)
- Start ferrum and send a direct message to the bot from your Telegram account.

## Local Dev Without Telegram
- `cp examples/local.mock.env .env`
- `cargo run -- chat --stdin`

## Storage and Cache
Recommended production topology:
- `Postgres`: source of truth for conversations, turns, plans, tasks, artifacts, reminders, traces.
- `Redis`: volatile read cache only. Ferrum does not use Redis as the durable queue or lock manager.

What Redis is useful for in ferrum:
- caching `recent_turns` to shorten repeated context loads
- caching latest summary and memory search results
- caching dashboard-heavy reads like recent runtime events, plan JSON, task JSON, and total cost
- reducing repeated JSON serialization for live operator views

## Core Commands
- `cargo run -- init`
- `cargo run -- run`
- `cargo run -- dashboard`
- `cargo run -- doctor`
- `cargo run -- identity lint`
- `cargo run -- skills lint`
- `cargo run -- replay <event_id>`
- `cargo run -- chat --stdin`
- `cargo run -- team status`
- `cargo run -- team simulate`
- `cargo run -- export-trace <conversation_id>`

## Identity
`IDENTITY.md` uses YAML frontmatter + required sections.

Required model routes:
- `fast`
- `reasoning`
- `tool_use`
- `vision`
- `reviewer`
- `planner`
- `fallback[]`

Required sections:
- Mission
- Persona
- Tone
- Hard Rules
- Do Not Do
- Escalation
- Memory Preferences
- Channel Notes
- Planning Principles
- Review Standards

## Skills
Each skill lives in `skills/<skill_name>/SKILL.md` and supports:
- `builtin`
- `command` (JSON stdin/stdout)
- `http` (allowlisted domains)

Shipped skills:
- `memory.write`
- `memory.search`
- `reminders.create`
- `reminders.list`
- `http.fetch` (disabled by default unless allowlisted)
- `agent.status`
- `agent.help`
- `quality.verify`
- `sample.command.echo`

## Add a Skill (Worked Example)
1. Create folder: `skills/quality.localcheck/`.
2. Add `skills/quality.localcheck/SKILL.md`.
3. If `kind: command`, place executable in same folder and set `entrypoint`.
4. Add skill name to `IDENTITY.md -> permissions.allowed_skills`.
5. Save file; ferrum hot-reloads skills automatically.
6. Validate with `cargo run -- skills lint`.

Minimal command-skill frontmatter example:
```yaml
---
name: quality.localcheck
version: 1.0.0
description: local checker
kind: command
entrypoint: check.sh
input_schema: { type: object }
output_schema: { type: object }
permissions: []
timeout_ms: 1000
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [quality]
triggers: [check]
---
```

## Team and Parallelism
Team behavior now has two layers:
- bootstrap defaults from environment on first start
- live runtime settings from dashboard `Config`

The dashboard can now manage:
- team size
- max parallel tasks
- ephemeral worker allowance and cap
- subagent mode
- roleset
- profile path
- review interval
- retry / review loop bounds
- plan size / depth bounds
- principal skills
- skill specialization by role and by persistent subagent

Ephemeral workers:
- are created only when needed
- are resource-aware
- are bounded by host CPU/memory pressure
- are destroyed on release

Role profile overlays example: `examples/sample_team_profiles/`.

## Dashboard
The dashboard is Telegram-first and mobile-first. It includes:
- `Home` for runtime overview
- `Flow` for the live execution canvas
- `Events` for incoming messages and runtime events
- `Config` for runtime team settings and specialization

## Dashboard Endpoints
- `/dashboard`
- `/dashboard/conversations/:id`
- `/dashboard/plans/:id`
- `/dashboard/tasks/:id`
- `/dashboard/team`
- `/dashboard/config`
- `/api/state`
- `/api/events`
- `/api/plans/:id`
- `/api/tasks/:id`
- `/api/team`
- `/api/config`
- `/api/flow`
- `/events/stream`
- `/healthz`
- `/readyz`
- `/metrics`

Operator actions:
- pause/resume runtime
- reload identity
- reload skills
- replay event
- toggle outbound kill switch
- apply team runtime settings

## Testing and Quality
Run:
- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`

## Docker + systemd
- Dockerfile: `docker/Dockerfile`
- Service unit: `docker/ferrum.service`

## Docs
- `docs/MASTERPLAN.md`
- `docs/ARCHITECTURE.md`
- `docs/THREAT_MODEL.md`
- `docs/OPERATIONS.md`
