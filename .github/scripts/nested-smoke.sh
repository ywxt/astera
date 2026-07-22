#!/usr/bin/env bash
set -euo pipefail

test_root="$(mktemp -d)"
runtime_dir="$test_root/runtime"
config_dir="$test_root/config"
log_file="$test_root/astera.log"
mkdir -m 700 "$runtime_dir"
mkdir -p "$config_dir"

compositor_pid=""
cleanup() {
    if [[ -n "$compositor_pid" ]]; then
        kill "$compositor_pid" 2>/dev/null || true
        wait "$compositor_pid" 2>/dev/null || true
    fi
    rm -rf "$test_root"
}
trap cleanup EXIT

cargo build -p astera --bin astera
XDG_RUNTIME_DIR="$runtime_dir" XDG_CONFIG_HOME="$config_dir" \
    xvfb-run -a target/debug/astera --backend winit >"$log_file" 2>&1 &
compositor_pid=$!

for _ in $(seq 1 100); do
    if grep -q '^WAYLAND_DISPLAY=astera-' "$log_file"; then
        exit 0
    fi
    if ! kill -0 "$compositor_pid" 2>/dev/null; then
        cat "$log_file"
        exit 1
    fi
    sleep 0.1
done

cat "$log_file"
echo "nested compositor did not publish a Wayland socket" >&2
exit 1
