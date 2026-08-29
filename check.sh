#!/bin/sh
# Everything that must be green before a commit. Run it locally; CI runs the same.
set -e

echo "→ fmt"
cargo fmt --all -- --check

echo "→ clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "→ test"
cargo test --workspace

# Invariant tests are the ones that must never be allowed to rot (Part 6 §116.3).
# Named explicitly so a rename or accidental #[ignore] shows up as a failure here
# rather than silently reducing coverage.
echo "→ invariants"
INVARIANTS="
placeholders_are_never_safe_to_read
self_written_content_cannot_support_a_claim
whole_file_spans_are_not_precise
policy_denials_are_never_retryable
"
for t in $INVARIANTS; do
    if ! cargo test --workspace "$t" -- --exact --nocapture 2>&1 | grep -q "test result: ok. 1 passed"; then
        echo "  MISSING OR FAILING INVARIANT TEST: $t" >&2
        exit 1
    fi
    echo "  ok $t"
done

echo
echo "all green"
