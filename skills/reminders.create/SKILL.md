---
name: reminders.create
version: 1.0.0
description: Create a durable reminder job
kind: builtin
entrypoint: reminders.create
input_schema:
  type: object
  properties:
    text: { type: string }
    due_at: { type: string, description: RFC3339 UTC timestamp }
  required: [text, due_at]
output_schema:
  type: object
permissions: []
timeout_ms: 1200
max_retries: 0
cache_ttl_secs: 0
idempotent: false
side_effects: writes reminder and scheduler job
tags: [reminders, scheduler, tasks]
triggers: [remind, reminder, follow up, follow-up]
---
## What it does
Schedules a durable reminder and enqueues reminder delivery.

## When to use
Use when user asks for a future reminder.

## When NOT to use
Do not use for immediate replies that require no scheduling.

## Input notes
`due_at` must be a valid RFC3339 timestamp.

## Output notes
Returns `{ ok, reminder_id, due_at }`.

## Failure handling
Fails on invalid timestamp, policy denial, or storage errors.

## Examples
{"text":"Pay internet bill","due_at":"2026-03-15T14:00:00Z"}
