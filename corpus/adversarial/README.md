# The adversarial corpus

The gate every write tool has to pass.

> The adversarial corpus must be green before any write tool ships, and only
> ever grows.
> — [CLAUDE.md](../../CLAUDE.md)

Each case is one attack on the write path in [`crates/tools`](../../crates/tools):
a workspace, a tool call, and the **exact error code** the call must produce.
The cases are data, not code — a new one is six lines of TOML and needs no new
test function, which is the only way a suite like this survives contact with a
solo project.

`cargo test -p marrow-tools` runs all of them
(`every_adversarial_case_produces_its_expected_refusal`).

## The rule

**Only ever grows.** Every security bug found adds a permanent case. Deleting
one deletes a defence somebody found the hard way, and
`the_corpus_only_ever_grows` fails if the count drops — raise that floor when
you add cases, never lower it.

A case is only allowed to change if the *rule* changed, and then the change
belongs in [DECISIONS.md](../../DECISIONS.md) with the reasoning, not in a
commit that quietly relaxes an expectation to make a test pass.

## Files

| File | What it attacks |
|---|---|
| `traversal.toml` | `..`, absolute paths, percent-encoded separators, Unicode characters that fold to a dot |
| `symlink.toml` | Symlinks out of the workspace, and the two swapped between validation and the write (TOCTOU) |
| `unicode.toml` | NFC/NFD and case collisions — one file with two identities (Part 7 §126 #14) |
| `names.toml` | Names the filesystem or a sync client would mangle: NUL, control characters, reserved device names, over-long paths |
| `protected.toml` | `.git`, dependency trees, the model directory |
| `stale.toml` | Writing over a file that changed since it was read (invariant #6) |
| `placeholder.toml` | Reading a cloud placeholder to satisfy the stale check (invariant #3) |
| `self_written.toml` | Writes that are *allowed* — and must come back `origin = SELF` |

## Adding a case

Append to whichever file matches, or start a new one. Anything ending `.toml`
in this directory is loaded.

```toml
[[case]]
id      = "traversal-parent-segment"          # unique, kebab-case, appears in failures
attacks = "path traversal"                    # from the vocabulary below
why     = "`..` arrives as data, from a model that read it in a document."
setup   = [{ kind = "file", path = "notes.md", contents = "the user's document" }]
protect = ["models"]                          # workspace-relative subtrees to protect
race    = "target_content_changes"            # optional; see below
input   = { op = "create_file", path = "notes.md", body = "x", precondition = "new" }
expect  = { outcome = "refused", code = "POL_DENIED", message_contains = "already exists" }
```

Then raise the floor in `the_corpus_only_ever_grows`.

### `expect`

A refusal names a code from
[`marrow_core::Code`](../../crates/core/src/error.rs) in its wire form. **"Should
fail" is not an expectation**: a traversal refused as `POL_DENIED` because the
name happened to be too long has not tested containment at all. Add
`message_contains` wherever two different rules share a code — it is the only
thing separating "refused because it escaped" from "refused because it was
stale".

```toml
expect = { outcome = "refused", code = "FS_PATH_ESCAPE_BLOCKED", message_contains = "resolves outside" }
expect = { outcome = "written", at = "notes/summary.md", contains = "Q2 revenue" }
```

Every `written` case is additionally asserted to come back `origin = SELF` and
unable to support a claim, and every `refused` case is asserted to have left
nothing at all outside the workspace.

### `attacks`

The vocabulary the coverage test asserts on, so a whole class cannot quietly
disappear:

`path traversal` · `symlink escape` · `toctou` · `unicode normalisation` ·
`case collision` · `name mangling` · `protected directory` · `stale write` ·
`cloud placeholder` · `self-poisoning` · `injection`

Adding a new class means adding it to that list in `crates/tools/src/corpus.rs`
as well.

### `setup`

Runs before the tool is called. The sandbox is:

```text
<tmp>/
├── workspace/   the workspace root — the only place a write may legitimately land
└── outside/     everything a case is trying to reach
```

| `kind` | Fields | Where `path` is relative to |
|---|---|---|
| `dir` | `path` | the workspace |
| `file` | `path`, `contents` | the workspace |
| `symlink` | `path`, `to` | `path` in the workspace, `to` **relative to the sandbox** — so `outside/secrets` escapes and `workspace/real.md` does not |
| `outside_dir` | `path` | `outside/` |
| `outside_file` | `path`, `contents` | `outside/` |

Parent directories are created for you. Non-ASCII names should be written with
`\uXXXX` escapes — `café.md` and `café.md` are indistinguishable in a
diff otherwise, and telling them apart is the entire point of those cases.

### `precondition`

What the caller claims is at the target, without the case having to know a
digest:

| Value | Meaning |
|---|---|
| `"new"` (default) | nothing is there |
| `"current"` | the digest the file has when the case starts — what an honest caller that just read it would hold |
| `{ digest_of = "text" }` | the digest of this literal text — the caller holding something already out of date |

### `race`

What another process does *after* validation and *before* the write. This is
the only thing a static filesystem cannot express, and it is what makes the
TOCTOU cases real rather than decorative.

| Value | What happens |
|---|---|
| `parent_becomes_symlink_out` | the destination directory is replaced by a symlink pointing out of the workspace |
| `target_becomes_symlink_out` | the target file is replaced by a symlink pointing out of the workspace |
| `target_content_changes` | someone edits the target file — the editor the user has open |

## What is not here yet

The corpus covers the **write path**. Several cases from the full list in
[TRACKER](../../TRACKER.md#adversarial-corpus) belong to subsystems that do not
exist yet and must be added when they do: hostile instructions inside a PDF,
zip-slip, decompression bombs, injection through OCR text and EXIF comments,
and a table with mismatched units. They are ingestion and parsing cases, not
write cases, and putting a placeholder here for them would only make the
coverage look better than it is.
