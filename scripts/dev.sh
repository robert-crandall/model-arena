#!/usr/bin/env bash
#
# Dev loop: Vite on :5173 for Solid 2.0 HMR + diagnostics, proxying /api and
# /healthz to the Rust server on :8080.
set -euo pipefail

cd "$(dirname "$0")/.."

if [ ! -f web/build/index.html ]; then
  echo "==> web/build is missing; building the frontend once so rust-embed compiles"
  (cd web && bun run build)
fi

echo "==> building rust-home-server server"
cargo build --bin rust-home-server

echo "==> starting rust-home-server server on :8080"
./target/debug/rust-home-server &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT

ready=
for _ in $(seq 1 50); do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo "rust-home-server server exited during startup - see output above" >&2
    exit 1
  fi
  if curl -fs -o /dev/null http://127.0.0.1:8080/healthz; then
    ready=yes
    break
  fi
  sleep 0.2
done

if [ -z "$ready" ]; then
  echo "rust-home-server server never became healthy on :8080 after 10s" >&2
  exit 1
fi

echo "==> starting Vite on :5173"
cd web && bun run dev
