# Ferrum Operations

## Startup Checklist
1. Copy `.env.example` to `.env`.
2. Set OpenRouter, Postgres, Redis, and Telegram credentials.
3. Validate identity and skills:
   - `cargo run -- identity lint`
   - `cargo run -- skills lint`
4. Run diagnostics:
   - `cargo run -- doctor`
5. Start runtime:
   - `cargo run -- run`

## Key Environment Variables
### Team/Supervisor
- `FERRUM_TEAM_SIZE`
- `FERRUM_MAX_PARALLEL_TASKS`
- `FERRUM_ALLOW_EPHEMERAL_SUBAGENTS`
- `FERRUM_MAX_EPHEMERAL_SUBAGENTS`
- `FERRUM_SUBAGENT_MODE`
- `FERRUM_SUBAGENT_ROLESET`
- `FERRUM_SUBAGENT_PROFILE_PATH`
- `FERRUM_SUPERVISOR_REVIEW_INTERVAL_MS`
- `FERRUM_MAX_REVIEW_LOOPS_PER_TASK`
- `FERRUM_MAX_TASK_RETRIES`
- `FERRUM_PLAN_MAX_TASKS`
- `FERRUM_PLAN_MAX_DEPTH`
- `FERRUM_REQUIRE_FINAL_REVIEW`

### Dashboard
- `FERRUM_ENABLE_DASHBOARD`
- `FERRUM_DASHBOARD_BIND`
- `FERRUM_DASHBOARD_AUTH_TOKEN`

### Safety
- `FERRUM_OUTBOUND_ENABLED`
- `FERRUM_OUTBOUND_KILL_SWITCH`
- `FERRUM_DRY_RUN`
- `FERRUM_HTTP_SKILL_ALLOWLIST`

### Channels
- `TELEGRAM_ENABLED`
- `TELEGRAM_BOT_TOKEN`
- `TELEGRAM_BOT_USERNAME`

- `FERRUM_DATABASE_BACKEND`
- `FERRUM_POSTGRES_URL`
- `FERRUM_CACHE_BACKEND`
- `FERRUM_REDIS_URL`
- `FERRUM_CACHE_NAMESPACE`

## Storage Operations
- Postgres is the authoritative store in production.
- Redis is disposable cache state; clear it safely during troubleshooting.
- If Redis is unavailable, ferrum should still operate against Postgres, just with slower hot reads.
- SQLite remains supported for local/offline mode.

## Health and Metrics
- `GET /healthz`
- `GET /readyz`
- `GET /metrics`

Main metrics:
- inbound events
- queue depth
- plan/task latencies
- review/integration latencies
- model/skill latency
- retries/failures
- token usage and estimated cost

## Dashboard Operations
- `/dashboard` for global overview
- `/events/stream` for live timeline
- operator actions:
  - pause/resume runtime
  - reload identity/skills
  - replay event
  - toggle kill switch

If auth token is configured, requests must include header `x-ferrum-dashboard-token`.

## Backups and Recovery
- Back up Postgres with normal physical or logical backups.
- Redis does not replace backups; it is cache state.
- If using SQLite local mode, stop the process before hot backup of `data/ferrum.db` (+ WAL/SHM).
- Recovery flow:
  1. restore Postgres snapshot or local SQLite snapshot
  2. optionally flush Redis if cache contents are stale
  3. run `cargo run -- doctor`
  4. replay critical events with `ferrum replay <event_id>` if needed

## Troubleshooting
- Invalid model routes: verify IDs in `IDENTITY.md` and rerun `doctor`.
- No outbound sends: check kill switch/dry-run/outbound flags.
- Skills not loading: run `skills lint`, inspect YAML frontmatter and schemas.
- Identity reload rejected: malformed file; runtime keeps last-known-good.
- Dashboard unauthorized: verify `x-ferrum-dashboard-token`.

## Deployment
### Docker
```bash
docker build -f docker/Dockerfile -t ferrum:latest .
docker run --env-file .env -p 8080:8080 ferrum:latest
```

### systemd
Use `docker/ferrum.service` as baseline. Keep `.env` outside repo, locked to operator-only permissions.
