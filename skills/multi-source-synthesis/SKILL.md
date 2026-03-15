---
name: multi-source-synthesis
version: 1.0.0
description: Guidance skill for merging evidence from multiple tracks into a single answer
kind: command
entrypoint: multi_source_synthesis.sh
input_schema:
  type: object
  properties:
    goal: { type: string }
    evidence_count: { type: integer }
output_schema:
  type: object
permissions: []
timeout_ms: 500
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [synthesis, integration, evidence]
triggers: [integrate, synthesize, merge, final answer]
---
## What it does
Provides guidance for integrating accepted artifacts and evidence into a coherent final answer.

## When to use
Use after research, validation, or market-data tracks have already produced artifacts.

## When NOT to use
Do not use before evidence exists or as a replacement for live-data acquisition.

## Input notes
Accepts the overall goal and optional evidence count.

## Output notes
Returns JSON with the recommended synthesis order, conflict handling, and confidence policy.

## Failure handling
Fails only if the command process cannot be executed.

## Examples
{"goal":"Combinar hallazgos sobre BTC/USD","evidence_count":5}
