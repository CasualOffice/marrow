#!/bin/sh
# Everything that must be green before a commit. Run it locally; CI runs the same.
set -e

echo "→ fmt"
cargo fmt --all -- --check

echo "→ clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "→ test"
cargo test --workspace

# One migration chain, one list. `Store::compose` rejects a chain that is
# unsorted, clashing or gapped, but a chain that merely stops early is
# well-formed — so a composition root that names a subset of the extensions
# compiles, passes its tests, and then refuses to open the database the other
# root wrote. That shipped: the CLI passed `fts5::MIGRATION` alone, and every
# `marrow search`, `marrow status` and `marrow mcp` against a real index died
# with CFG_UNSUPPORTED_VERSION while the suite stayed green.
echo "→ one migration chain"
stray=$(grep -rn 'fts5::MIGRATION\|vector::MIGRATION' crates --include='*.rs' \
        | grep -v '^crates/index/src/lib.rs:' | grep -v ':[0-9]*: *//' || true)
if [ -n "$stray" ]; then
    echo "  a migration chain is being assembled outside marrow_index::MIGRATIONS:" >&2
    echo "$stray" >&2
    exit 1
fi
echo "  ok"

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
a_file_this_system_wrote_is_indexed_as_self_written_and_cannot_be_cited
the_chunks_carry_the_same_origin_as_the_file
the_record_survives_a_reindex
a_file_the_user_edits_becomes_theirs_again
a_created_file_is_recorded_as_self_written_not_merely_reported_as_such
a_fetch_that_needs_confirmation_is_refused_rather_than_silently_granted
a_zero_or_empty_vector_has_no_direction_and_is_refused
vectors_of_different_widths_are_not_compared_at_all
changing_the_embedding_model_discards_every_vector
a_chunk_only_the_semantic_branch_found_is_still_rendered_and_citable
a_vector_for_a_chunk_the_store_no_longer_has_is_dropped_not_rendered
search_still_works_with_no_embeddings_at_all
the_result_says_which_branches_actually_ran
the_batch_size_is_a_working_size_not_a_round_number
the_stopwords_are_dropped_because_the_query_is_disjunctive
a_later_store_migration_does_not_reorder_an_earlier_extension
two_migrations_claiming_one_version_is_refused_not_resolved
the_heading_chain_goes_in_with_the_body
a_tombstoned_chunk_is_not_queued_for_embedding
byte_offsets_map_to_utf16_indices
an_astral_character_costs_two_utf16_units
a_run_of_nothing_has_no_box_rather_than_a_box_at_the_origin
a_file_named_pdf_that_is_not_one_is_handed_on
the_pinned_resolver_ignores_the_name_it_is_given
the_reported_line_is_where_the_match_is_not_where_the_chunk_starts
the_line_can_never_leave_the_chunk_it_came_from
the_answer_budget_is_what_the_window_has_left_not_a_flat_number
a_large_prompt_does_not_starve_the_answer_when_the_memory_is_there
a_machine_with_no_memory_free_still_gets_a_usable_floor
a_superseded_version_is_not_searchable_even_though_its_index_row_survives
a_deleted_file_is_not_searchable_even_though_its_index_row_survives
the_excerpt_is_the_files_text_even_when_the_filename_is_what_matched
a_run_interrupted_between_the_version_row_and_its_chunks_recovers_on_the_next_run
reporting_health_does_not_claim_the_index_was_checked
the_current_question_is_marked_and_earlier_ones_are_not
the_migration_is_idempotent_and_records_its_version
every_persisted_cell_carries_a_source_span
every_cell_keeps_a_precise_span_that_resolves_to_its_own_bytes
a_markdown_table_and_a_csv_arrive_as_the_same_thing
a_table_that_failed_reconstruction_is_still_discoverable_as_text
a_walk_that_could_not_open_a_directory_does_not_conclude_anything_is_gone
a_file_that_comes_back_is_searchable_again
the_chunk_count_excludes_what_search_can_no_longer_return
a_cloud_only_file_is_skipped_unread_and_counted
a_hit_in_a_file_this_system_wrote_is_not_citable
stopping_early_is_reported_rather_than_looking_like_an_exhaustive_answer
changes_made_while_the_app_was_closed_are_picked_up_when_it_opens
watching_is_recorded_where_another_process_can_read_it
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

# Every `var(--x)` in the UI resolves to something.
#
# An undefined custom property is not an error anywhere in the stack: CSS drops
# the declaration at computed-value time, the bundler never looks inside a
# `var()`, and TypeScript cannot see a stylesheet. The control simply renders
# without the thing, which is why this class of bug is found by eye months
# later. `--raised` and `--r-md` on the Status page's Add-workspace button left
# it with no plate and square corners in an app where every control is
# `--r-ctrl`, and it was the third instance in a month (UI_AUDIT §4).
echo "→ every css var resolves"
UI=crates/desktop/ui/src
# Set from TSX with an inline `style`, so no stylesheet will ever define them.
# One entry per property, naming who sets it — anything not on this list and not
# in a stylesheet is a typo or a phantom token:
#   --result-row-h  ResultList.tsx:98      (virtualiser row height)
#   --artifact-w    ArtifactPanel.tsx:800  (drag-resized panel width)
#   --zoom          ArtifactPanel.tsx:554  (artifact zoom factor)
INLINE='--result-row-h|--artifact-w|--zoom'
defined=$(grep -rho '^[[:space:]]*--[A-Za-z0-9_-]*[[:space:]]*:' "$UI" --include='*.css' \
          | tr -d ' \t:' | sort -u)
used=$(grep -rho 'var([[:space:]]*--[A-Za-z0-9_-]*' "$UI" --include='*.css' \
       | sed 's/.*--/--/' | sort -u)
undefined=
for name in $used; do
    if echo "$defined" | grep -qx -- "$name"; then continue; fi
    if echo "$name" | grep -qxE -- "$INLINE"; then continue; fi
    undefined="$undefined $name"
done
if [ -n "$undefined" ]; then
    echo "  used in a stylesheet, defined nowhere in $UI and not set inline:" >&2
    for name in $undefined; do
        grep -rn "var([[:space:]]*$name[[:space:]]*[,)]" "$UI" --include='*.css' >&2
    done
    exit 1
fi
echo "  ok"

echo
echo "all green"
