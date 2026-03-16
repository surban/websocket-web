#!/bin/bash
#
# Runs the tests.
#

set -e

cd "$(dirname "$0")"

export CHROMEDRIVER=chromedriver
export WASM_BINDGEN_USE_BROWSER=1

# Kill any existing process on port 8765.
fuser -k 8765/tcp 2>/dev/null || true

# Start test server in background.
echo "Starting test server..."
pushd test-server > /dev/null
RUST_LOG=info cargo run --release &
SERVER_PID=$!
popd > /dev/null
trap "kill $SERVER_PID 2>/dev/null" EXIT

# Wait for test server to be ready.
for i in $(seq 1 30); do
    if nc -z 127.0.0.1 8765 2>/dev/null; then
        break
    fi
    sleep 0.1
done

echo ""
echo "=== Running tests ==="
cargo test --release --test example --test tests -- --nocapture
echo ""

echo "=== Running speed tests ==="
for test in send_stream send_standard recv_stream recv_standard both_stream; do
    echo "--- $test ---"
    cargo test --release --test speed -- "$test" --nocapture
    echo ""
done

echo "All tests passed."
