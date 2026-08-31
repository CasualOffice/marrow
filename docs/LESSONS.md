# Lessons

Bug shapes that are not specific to this codebase. Marrow supplied the evidence
— every case has a commit and a number behind it — but none of the shapes are
about Rust, about search, or about this repo. They are about values nobody
reads, checks that cannot fail, tests that look like coverage, and comments that
were true once. The transferable part is the shape and the question it makes you
ask; the Marrow case is only there so you can watch it happen to real code.
Ordered by how often the shape recurred, not by severity.

---

## 1. A value written faithfully and never read back

**The shape.** A field is persisted on every write, its doc comment describes
the *mechanism* it powers, and nothing consumes it. The type system is
satisfied: the value has a writer, a column, and a name that reads as an
explanation. The mechanism does not exist.

**The case.** Three times in one week. `parser_version` was specified as "the
mechanism by which an upgrade schedules reprocessing", written with every parse
result, and read back only for display — the staleness gate compared content
hashes, so a parser fix reached no file already indexed, which on a 35,000-file
index is the whole corpus (`093d375`). Then `CHUNKER_VERSION`, documented as
"persisted so a change can schedule re-chunking", written with every chunk, and
never compared, because the gate had been taught about parsers only (`654053f`).
Then the routing decision itself: a file that fell through the chain to the
metadata fallback recorded `metadata` as its parser, and the metadata parser
never changes, so nothing was ever stale and the file kept a metadata-only
result permanently after a real parser shipped. On the author's corpus, 26
spreadsheets, 25 Word documents, 18 OpenDocument files and 11 images sat with no
content and no tables while working parsers existed (`435f490`).

**Why it was invisible.** Each was correct at the write site, which is where you
look. Nothing errors, nothing logs, and the doc comment is a load-bearing claim
no compiler, linter or test checks.

**What to do.** Grep for the reader when you add the writer, in the same change.
If there is no reader yet the comment must say so — "written for a future gate,
not yet consumed" would have caught all three. A doc comment describing a
mechanism is an assertion someone owes a test.

## 2. Green locally, red in CI, and the script was identical

**The shape.** A local check claims parity with CI. The script really is
identical. The *tree* it runs against is not, because the difference is
untracked build output that exists only on a developer's disk.

**The case.** `check.sh` opens with "Run it locally; CI runs the same."
`tauri::generate_context!()` reads `ui/dist` at *compile* time and panics in the
proc macro when it is absent; `ui/dist` is build output, so it is gitignored, so
a clean checkout lacks it — and `ci.yml` went from checkout straight to
`./check.sh`. The desktop crate had therefore never compiled in CI: twelve runs,
five failures, seven cancellations, zero successes, for the life of the repo
(`59de1c7`). The same ordering fault sat in the release workflow, unnoticed
because it is tag-triggered and the repo had never been tagged (`1e89afa`,
`5c99ac2`).

**Why it was invisible.** The developer machine is green for a reason that has
nothing to do with the commit. A claim of parity is worse than no claim, because
it is what stops you checking.

**What to do.** Write down what the local check assumes exists and CI does not:
build output, caches, toolchains, a populated database, a credential in a
keychain. Run the claim of parity from a clean clone occasionally. And if two
pipelines run "the same steps", compare them — these diverged and nothing
noticed.

## 3. Reading a CI log is not reproducing a CI failure

**The shape.** The log names a real failure. You fix it. The build goes red
again, elsewhere. The thing underneath was never the thing in the log.

**The case.** Three fixes were pushed from log-reading alone on the way down to
§2, each a genuine bug: a `.gitignore` rule with a bare `secrets.*` pattern
silently eating `crates/model/src/secrets.rs`, so `git add` skipped it and every
clone was broken while the local build stayed green (`fa3be7d`); and
`clippy::byte_char_slices`, which landed in Rust 1.98 while local stable was
1.96, so the lint could not fire locally (`cac9928`). Each fix produced another
red build. The real cause was found only by moving `ui/dist` out of the tree and
watching the proc-macro panic happen locally.

**Why it was invisible.** A log shows the first thing that failed, not the first
thing that was wrong, and fixing from one is a guess that feels like a diagnosis
because the guess is usually a real defect.

**What to do.** Reproduce the environment difference locally before pushing a
fix: delete the artifact, downgrade the toolchain, clone into a fresh directory.
Two consecutive log-driven fixes is the signal to stop guessing.

## 4. A test that pins one arm of a two-arm condition reads as coverage

**The shape.** A guard is `A && B`. A named test pins `A`. To anyone scanning
the list, the guard looks covered.

**The case.** `a_cancelled_sweep_never_concludes_that_the_files_it_missed_are_gone`
pins the `cancelled` half of `!outcome.cancelled && outcome.failures.is_empty()`
in the reconciliation pipeline. Both criticals from an adversarial review lived
in the other half: a walker error was only `debug!`-logged and never reached
`failures`, so every file under an unreadable directory was soft-deleted while
sitting on disk and reported as `removed: N, failed: 0`; and nothing ever set a
file's status back to `ACTIVE`, so every delete was permanent including a wrong
one (`2696599`).

**Why it was invisible.** The test name is a true sentence about the guard's
*purpose*. Coverage tooling counts the line as covered, because it is.

**What to do.** Name a test after the arm it pins, not the invariant it
protects. Mutation-verify: revert the fix, run the test, watch it fail. Every
fix in the following review round was checked that way.

## 5. Borrowing a nearby guard without asking what it guarded against

**The shape.** New code needs a precondition. There is a guard right there,
already tested, obviously about the same run. You reuse it. It answers a
different question.

**The case.** Caused directly by §4. The routing fingerprint from §1 was
recorded under the bulk delete's guard. The delete asks *did this walk establish
what is gone*, which any failure invalidates, because an unopenable directory
removes its whole subtree from the seen set. The fingerprint asks *was every
file offered to the current parser chain*, and a file that was offered and
failed to parse was still offered. Three spreadsheets tripping a UNIQUE
constraint kept `failures` non-empty for ever, so the fingerprint was never
written and every sweep re-routed all 34,000 files — the exact non-convergence
the mechanism existed to prevent. The fix keeps walk errors as their own fact:
folded into `failures`, an unopenable directory is indistinguishable from a
spreadsheet that will not parse, and those mean different things. The corpus went
from re-parsing 13,307 files a sweep to 34,636 unchanged in 2.2 s (`7b9612c`).

**Why it was invisible.** The borrowed guard is *more* conservative, so the bug
is never a wrong answer, only work that never converges — and the existing test
could not see it: its fixture has no file that fails.

**What to do.** Before reusing a condition, write down in words the question it
answers. A different sentence needs a different condition, even when the two
agree on every input you have — and check your fixture can tell them apart. A
fixture where both guards agree proves nothing about the one you picked.

## 6. The platform can hide a bug instead of exposing it

**The shape.** Code makes a request so wrong it should fail loudly. The runtime
forgives it, for reasons that are accidents, so nothing surfaces and no
measurement finds it.

**The case.** `grade()` decided whether a table reconstructed exactly by
painting every square of its bounding box into a `Vec<bool>`. A workbook holding
two cells — one in A1, one in the far corner — asked the allocator for 17.2 GB
on every index run. It never crashed, for two reasons that are both accidents:
`vec![false; n]` allocates zeroed, so macOS maps the pages lazily and nothing
touches them, and `all()` short-circuits on the first hole, which in a sparse
table is index 1. Peak RSS for that request is 12 MB (`ee6c87a`).

**Why it was invisible.** Every observable was healthy. No memory measurement
could have found it, because residency is what forgave it; an allocator that
refused would have aborted the process uncatchably.

**What to do.** Assert on the request, not the outcome — the regression test
counts bytes *requested* through a global allocator, because the request is the
defect. Where the platform is being generous (lazy allocation, short-circuit
evaluation, a filesystem tolerating what another would not), that generosity is
not a guarantee, and the assertion belongs on the thing you control.

## 7. An address is not a size

**The shape.** A format allows a huge address space. One item at a far address
makes the *bounding box* enormous while the data stays tiny. Code that iterates
the box does work proportional to a number the file merely mentioned.

**The case.** XLSX allows 1,048,576 rows and 16,384 columns. Three separate
places walked the box rather than the cells: grading (§6), chunking — roughly
51 GB of chunk text from two cells, with no ceiling and outside any budget — and
the schema listing, one line per column of the box, each line scanning every
cell for its numeric range (`654053f`). The XLSX parser had already made the
argument against this, in a comment about work "proportional to an address the
file merely mentioned", and every one of its callers did it anyway.

**Why it was invisible.** The file opens instantly in Excel, so nothing looks
pathological — and the rule had been learned, written down, in the right module,
by someone who understood it.

**What to do.** A rule learned in one module does not propagate to its callers
by being written down near where it was learned. Put the constraint in the type
you hand out — cells, not dimensions — so callers cannot rebuild the box. Where
that is not possible, fix the other consumers of the same shape in the same
change. Anything with sparse addressing has this waiting: spreadsheets, sparse
matrices, paginated APIs reporting a total, ID spaces read as counts.

## 8. A number that is true and is read as a claim

**The shape.** A figure is accurate about what the code did, and is displayed
where a reader takes it as a fact about their data.

**The case.** `chunks.status` has a `SUPERSEDED` value nothing has ever written,
so every count over that column included text no search could return: 274,519
chunks counted against 59,197 reachable, a 4.6× over-report on every surface.
Worse than a wrong number — `marrow embed` derived its work from the same
predicate and offered a two-hour job of which four fifths would embed
unretrievable text, while `marrow status` suggested running it, and the coverage
figure divided by the inflated denominator and read 1.26% where the truth is 4%
(`fc3d9bb`). Separately, a match count taken off a bounded retrieval was
rendered as a corpus count; it now says "at least 100 matches without the
filters", because any number that looks like a total here is a floor.

**Why it was invisible.** The number is not wrong. It is a true statement about a
query presented as one about a corpus, and the reader cannot see the difference.

**What to do.** For every number on a user-facing surface, say what population
it counts, and check that the predicate producing it is the predicate the
feature uses. A count of searchable things that disagrees with what searching
returns is not a count of anything. When the honest answer is a lower bound,
render it as one.

## 9. What works from a source checkout is not what ships

**The shape.** The application is correct and the artifact is not. Nothing in
the source tree tells you, and no unit test is in a position to.

**The case.** The released app could neither load a model nor read a folder
(`f496966`). The Python worker was never declared as a bundle resource, so it
was not in the `.app`; the lookup fell back to a path baked in at compile time
from `CARGO_MANIFEST_DIR` — on a release build, a directory on a CI runner that
has never existed on a user's machine. Discovery still reported healthy, because
it checked only for a Python interpreter. Separately, no macOS
usage-description strings were declared, so TCC refuses every read inside
Downloads, Documents and Desktop, with no prompt and no error distinguishable
from an empty folder: granting a folder indexed zero files and looked like an
index that had not started.

**Why it was invisible.** Both faults are total in a bundle and absent from
`cargo run`, where the resource sits at its source path and the process is not
sandboxed. Both live in packaging config, which no test imports.

**What to do.** Verify the artifact, not the config: build the bundle and check
the file is where the shipped lookup will look. Treat a fallback resolving to a
build-machine path as a bug on sight. Where the OS can deny you silently,
discovery must require every half it depends on and name the missing one — "the
runtime stopped" was a sentence about a process that never started.

## 10. A trade that was right and quietly stopped being right

**The shape.** A decision is justified by a quantity, the reasoning is written
down honestly, and the quantity moves.

**The case.** Pre-migration backups were kept for ever, with a comment saying
so: "nothing prunes these yet: an M1 database is a few megabytes and a lost
backup is unrecoverable, so keeping them all is the cheap side of the trade."
True when written. On a real corpus the index reached 4.3 GB and its four kept
backups came to 4.2 GB — most of what filled a disk. A full disk stops SQLite
writing, so the mechanism that exists to protect the index was the thing taking
it down (`26545fb`).

**Why it was invisible.** The comment never becomes wrong. It describes
conditions, and nothing watches the conditions.

**What to do.** A comment justifying a decision by a quantity should name the
quantity at which it stops holding — "cheap while the database is under a
hundred megabytes" is a tripwire a reader can check. Better, enforce the bound
in code, as the fix did by keeping two backups instead of all of them. Two, not
one: a schema fault is often noticed a migration late.

## What to actually do differently

- **Mutation-verify a test** — revert the fix, watch it fail. The gaps behind §4
  and §5 were in tests nobody had done this to.
- **Name a test after the arm it pins**, not the invariant it protects.
- **Grep for the consumer when you add a persisted field**, and say in the
  comment when there is not one yet. §1 is that missing grep, three times.
- **When a local check passes and CI fails, distrust the local check first** and
  ask what CI's tree lacks. Two log-driven fixes in a row means stop and
  reproduce the environment.
- **Write down the question a guard answers before reusing it.**
- **Assert on the request, not the outcome**, where the platform can forgive you.
- **Iterate the items, never the address space** — and fix the callers in the
  same change you learned it in.
- **State the population behind every number you show.**
- **Build the artifact and check it**, especially anything resolved by path at
  runtime or granted by the OS.
- **Put the number in the comment that justifies a decision by a number.**
