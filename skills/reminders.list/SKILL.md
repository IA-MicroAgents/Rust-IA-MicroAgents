---
name: reminders.list
version: 1.0.0
description: List reminders for current user
kind: builtin
entrypoint: reminders.list
input_schema:
  type: object
  properties:
    limit: { type: integer, minimum: 1, maximum: 50 }
output_schema:
  type: object
permissions: []
timeout_ms: 800
max_retries: 0
cache_ttl_secs: 5
idempotent: true
side_effects: none
tags: [reminders, scheduler, list]
triggers: [list reminders, pending reminders, reminders]
---
## What it does
Returns pending/sent reminder records for the current user.

## When to use
Use when user asks what reminders exist.

## When NOT to use
Do not use as a substitute for creating or editing reminders.

## Input notes
`limit` optional.

## Output notes
Returns `{ items: [{id,text,due_at,status}] }`.

## Failure handling
Fails on query errors.

## Examples
{"limit":10}
