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
every_ir_node_has_a_source_span
file_text_is_untrusted_never_deterministic
an_injection_attempt_in_a_file_is_still_just_untrusted_text
the_chain_always_terminates_in_success
a_parser_panic_does_not_escape_the_router
code_symbols_are_never_split
byte_spans_round_trip
budget_exceeded_degrades_not_panics
a_cloud_placeholder_is_never_content_parsed
index_and_canonical_write_share_one_transaction
derived_index_is_rebuildable_from_canonical
query_syntax_cannot_be_injected
literal_search_refuses_non_resident_files
hardlinks_are_distinct_files_and_the_index_converges
every_discovered_file_is_actually_stored
a_watch_error_demands_a_rescan_rather_than_being_swallowed
every_unhealthy_state_can_explain_itself
compound_enum_names_get_their_underscores
every_outcome_and_tier_satisfies_its_check_constraint
every_artifact_the_parser_can_produce_is_persistable
every_persisted_enum_satisfies_its_check_constraint
persisted_warnings_are_valid_json
a_file_that_cannot_be_read_is_still_recorded_from_its_metadata
the_failure_headline_always_equals_the_sum_of_its_groups
the_index_and_the_canonical_chunks_agree_after_a_full_run
a_symlink_inside_scratch_cannot_be_used_to_climb_out
a_symlinked_target_file_is_refused_even_when_its_parent_is_inside
a_model_area_that_reaches_an_indexed_folder_through_a_symlink_is_refused
a_symlink_created_after_validation_is_still_refused
the_stale_check_runs_at_commit_time_not_at_validation_time
everything_written_is_marked_self_written_and_cannot_be_cited
every_adversarial_case_produces_its_expected_refusal
the_corpus_only_ever_grows
a_hostname_that_resolves_to_loopback_is_refused
a_redirect_that_lands_on_a_private_address_is_refused
a_redirect_that_downgrades_the_scheme_is_refused
untrusted_content_is_never_the_last_block
a_delimiter_collision_regenerates_rather_than_escaping
self_written_evidence_is_dropped_and_the_omission_is_visible
a_worker_that_breaks_its_memory_budget_is_stopped_mid_answer
a_prefix_is_never_reused_across_a_classification_boundary
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
