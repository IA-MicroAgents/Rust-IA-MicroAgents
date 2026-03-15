#!/bin/sh
set -eu
payload=$(cat)
printf '{"ok":true,"echoed":%s}\n' "$payload"
