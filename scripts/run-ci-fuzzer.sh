#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: run-ci-fuzzer.sh <target> <corpus> [libfuzzer arguments...]" >&2
  exit 2
fi

target="$1"
corpus="$2"
shift 2

# cargo-fuzz may leave sanitizer helpers alive after the fuzzer exits. GitHub's
# hosted runner then stalls while cleaning those orphan processes even though
# every workflow step has completed. Isolate the fuzzer in its own process
# group and reap the whole group before this step returns.
setsid cargo +nightly fuzz run --fuzz-dir fuzz "$target" "$corpus" -- "$@" &
fuzz_group="$!"

cleanup() {
  kill -TERM -- "-$fuzz_group" 2>/dev/null || true
  for _ in {1..20}; do
    if ! kill -0 -- "-$fuzz_group" 2>/dev/null; then
      return
    fi
    sleep 0.1
  done
  kill -KILL -- "-$fuzz_group" 2>/dev/null || true
}
trap cleanup EXIT

if wait "$fuzz_group"; then
  fuzz_status=0
else
  fuzz_status=$?
fi

exit "$fuzz_status"
