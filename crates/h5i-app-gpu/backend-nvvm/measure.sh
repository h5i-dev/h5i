#!/usr/bin/env bash
# Compatibility entry point; application probes live with the application.
exec bash "$(dirname "$0")/../../../examples/app/parallel/gpu/measure.sh" "$@"
