# AI MicroAgents Threat Model

## Assets
- OpenRouter API key
- Telegram bot token
- User message history, memory facts, task artifacts
- Operator controls (pause/resume, replay, kill switch)

## Trust Boundaries
- inbound Telegram polling / Meta webhook traffic
- outbound OpenRouter and Telegram APIs
- optional command/http skills
- dashboard/operator API endpoints
- local filesystem (`IDENTITY.md`, `skills/`)
- Postgres and Redis network boundaries

## Abuse Cases
1. Webhook replay/spam floods queue.
2. Prompt injection attempts to bypass permissions.
3. Malformed LLM JSON tries to trigger unsafe tool behavior.
4. Command/HTTP skill misuse for data exfiltration.
5. Dashboard operator endpoint abuse.
6. Secret/PII leakage in logs or traces.
7. Unbounded plan/review loops causing runaway cost.

## Mitigations
- Event dedupe and immediate persistence.
- Bounded queues and bounded worker concurrency.
- Strict JSON parsing + repair attempt + safe fallback.
- Skill schema validation (input/output), timeout, retries, permission checks.
- HTTP domain allowlist.
- Outbound kill switch and dry-run mode.
- Dashboard token auth.
- Redaction of sensitive fields in logs/config views.
- Bounded plan size/depth, retries, review loops, and budget limits.
- Full audit trail in the primary database (`runtime_events`, `tool_traces`, `outbound_messages`, task review tables).

## Residual Risks
- Misconfigured allowlists can still expose data.
- Command skills remain high risk if enabled carelessly.
- Postgres credential compromise exposes conversation/task traces.
- Redis compromise exposes cached conversation fragments and operator snapshots.
- Token auth alone is weak if dashboard is internet-exposed without reverse-proxy hardening.

## Hardening Recommendations
- Add webhook signature verification.
- Put dashboard behind reverse proxy + mTLS/IP allowlist.
- Rotate OpenRouter and Telegram tokens regularly.
- Encrypt backups at rest.
- Add CI checks for identity/skills schema and forbidden settings.
