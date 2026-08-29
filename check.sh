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
# `--exact` needs a fully-qualified path, which couples this list to module
# layout. Filter by bare name instead and require a line reporting it passed —
# that catches both a rename and a silent #[ignore].
INVARIANTS="
placeholders_are_never_safe_to_read
self_written_content_cannot_support_a_claim
whole_file_spans_are_not_precise
policy_denials_are_never_retryable
exactly_one_current_version_per_file
job_idempotency_key_makes_reenqueue_a_noop
path_is_never_identity
backup_exists_before_a_migration_runs
crash_mid_transaction_leaves_no_partial_state
expired_job_leases_return_the_job_to_pending
symlink_escape_blocked
nfc_nfd_single_identity
placeholder_never_hydrated
every_noise_directory_is_excluded_not_just_the_last
"
missing=0
for name in $INVARIANTS; do
    if cargo test --workspace "$name" 2>&1 | grep -qE "^test .*${name} \.\.\. ok$"; then
        echo "  ok $name"
    else
        echo "  MISSING OR FAILING INVARIANT TEST: $name" >&2
        missing=1
    fi
done
[ "$missing" -eq 0 ] || exit 1

echo
echo "all green"
