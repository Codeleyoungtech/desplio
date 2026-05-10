#!/usr/bin/env bash
set -euo pipefail

export RUST_LOG="${RUST_LOG:-info}"
exec cargo run -p desplio-daemon
