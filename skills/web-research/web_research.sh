#!/bin/sh
set -eu
cat >/dev/null
printf '%s\n' '{"ok":true,"skill":"web-research","stage":"inspect_then_summarize","guidance":["fetch provided URLs first","extract title and compact evidence snippets","preserve source attribution in the synthesis","flag missing or unreachable sources instead of hallucinating"]}'
