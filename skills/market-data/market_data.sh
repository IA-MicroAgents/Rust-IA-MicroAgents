#!/bin/sh
set -eu
cat >/dev/null
printf '%s\n' '{"ok":true,"skill":"market-data","stage":"evidence_first","required_sources":["coingecko","coinbase","binance"],"guidance":["fetch live snapshots before reasoning","separate data acquisition from market interpretation","do not answer with pure priors when current data is required"]}'
