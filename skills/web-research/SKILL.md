---
name: web-research
version: 1.0.0
description: Guidance skill for URL and web evidence inspection before synthesis
kind: command
entrypoint: web_research.sh
input_schema:
  type: object
  properties:
    query: { type: string }
    urls:
      type: array
      items: { type: string }
output_schema:
  type: object
permissions: []
timeout_ms: 500
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [web, research, urls, evidence]
triggers: [url, noticia, latest, web research]
---
## What it does
Provides guidance for inspecting URLs and extracting evidence before any downstream analysis.

## When to use
Use when the user gives URLs or asks for current information that should be grounded in fetched documents.

## When NOT to use
Do not use when the answer can be produced from stable context already present in the conversation.

## Input notes
Accepts a query and optional URL list.

## Output notes
Returns a JSON policy with the expected sequence for fetch, extract, compare, and synthesize.

## Failure handling
Fails only if the command process cannot be executed.

## Examples
{"query":"Resume esta nota","urls":["https://example.com/report"]}
