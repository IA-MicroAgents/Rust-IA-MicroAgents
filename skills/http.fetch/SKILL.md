---
name: http.fetch
version: 1.0.0
description: Fetch HTTP resources from allowlisted domains
kind: builtin
entrypoint: http.fetch
input_schema:
  type: object
  properties:
    url: { type: string }
    method: { type: string }
    body: {}
    timeout_ms: { type: integer, minimum: 100, maximum: 15000 }
  required: [url]
output_schema:
  type: object
permissions: []
timeout_ms: 6000
max_retries: 1
cache_ttl_secs: 5
idempotent: true
side_effects: outbound network request
tags: [http, fetch, api]
triggers: [fetch, api call, http]
---
## What it does
Performs an outbound HTTP request with strict domain allowlist checks.

## When to use
Use for controlled API lookups on trusted domains.

## When NOT to use
Do not use when domain is not allowlisted or when request is risky.

## Input notes
`url` required; host must be allowlisted.

## Output notes
Returns `{ status, body }`.

## Failure handling
Fails closed on allowlist, timeout, or response parse violations.

## Examples
{"url":"https://example.com/status","method":"GET"}
