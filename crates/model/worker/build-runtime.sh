#!/usr/bin/env bash
# Build the relocatable MLX runtime that the app installs on first run.
#
# The runtime is *not* a venv. A venv records the interpreter that made it —
# `pyvenv.cfg` names an absolute `home`, and `bin/python` is a symlink out to
# it — so a venv built here would point at this machine's Homebrew and be
# useless anywhere else. That is the bug this script exists to stop repeating:
# every release so far shipped the worker script and left the interpreter as a
# thing the author happened to have.
#
# python-build-standalone's `install_only` distribution is self-contained and
# already relocatable. Packages go into its own `lib/python3.11/site-packages`,
# and the worker is invoked as `bin/python3.11 mlx_worker.py`, never through a
# console script — pip bakes an absolute shebang into those and we would be
# back where we started.
#
# Output: marrow-runtime-<VERSION>-macos-arm64.tar.gz and its sha256, which is
# what gets pinned in `crates/model/src/runtime.rs`.
set -euo pipefail

VERSION="${VERSION:-1}"
PY_TAG="20260901"
PY_VER="3.11.16"
OUT_DIR="${OUT_DIR:-$(pwd)/dist}"

# Pinned, all three. `pip install mlx-lm` today resolves to whatever PyPI
# serves today; one day that is a version where `LRUPromptCache` has moved and
# the failure is an ImportError at load with no version number in it.
PINS=(
  "mlx==0.32.2"
  "mlx-lm==0.31.3"
  "mlx-embeddings==0.1.0"
)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "==> cpython ${PY_VER} (python-build-standalone ${PY_TAG})"
curl --fail --location --progress-bar \
  "https://github.com/astral-sh/python-build-standalone/releases/download/${PY_TAG}/cpython-${PY_VER}%2B${PY_TAG}-aarch64-apple-darwin-install_only.tar.gz" \
  -o "$work/cpython.tar.gz"
tar -xzf "$work/cpython.tar.gz" -C "$work"
root="$work/python"
test -x "$root/bin/python3.11"

echo "==> pinned wheels"
# --no-compile: .pyc files are ~20% of the payload, are regenerated on first
# import anyway, and embed absolute paths in their tracebacks.
"$root/bin/python3.11" -m pip install --no-cache-dir --no-compile --upgrade pip
"$root/bin/python3.11" -m pip install --no-cache-dir --no-compile "${PINS[@]}"

echo "==> prove it before shipping it"
# Both halves. A runtime that generates and cannot embed is the failure mode
# the old setup hint produced, and it is invisible until semantic search is
# quietly worse than it should be.
"$root/bin/python3.11" - <<'PY'
import mlx.core as mx
from mlx_lm import stream_generate           # noqa: F401
from mlx_lm.models.cache import can_trim_prompt_cache  # noqa: F401
from mlx_embeddings import load              # noqa: F401
print("mlx", mx.__version__ if hasattr(mx, "__version__") else "ok")
PY

echo "==> strip what never runs"
# pip, setuptools and the test suite are build-time only. Nothing in the worker
# imports them and the runtime is never installed into again.
rm -rf "$root/lib/python3.11/site-packages/pip" \
       "$root/lib/python3.11/site-packages/pip-"*.dist-info \
       "$root/lib/python3.11/site-packages/setuptools" \
       "$root/lib/python3.11/site-packages/setuptools-"*.dist-info \
       "$root/lib/python3.11/test" \
       "$root/lib/python3.11/idlelib" \
       "$root/lib/python3.11/tkinter"

# **Every console script.** pip writes an absolute shebang into each one —
# `#!/var/folders/…/tmp.XXXX/python/bin/python3.11` — so `bin/` is where the
# build machine gets baked into the tree. The worker is invoked as
# `bin/python3.11 mlx_worker.py` and nothing here ever runs `mlx_lm.chat`, so
# they are dead weight that happens to also be the relocatability bug.
find "$root/bin" -type f ! -name 'python*' -delete
find "$root" -name '__pycache__' -type d -prune -exec rm -rf {} + 2>/dev/null || true
find "$root" -name '*.pyc' -delete 2>/dev/null || true

# `bin/python` is what `Runtime::discover` looks for. install_only ships
# python3 -> python3.11 but not always a bare `python`.
ln -sf python3.11 "$root/bin/python"
ln -sf python3.11 "$root/bin/python3"

echo "==> relocatability check"
# An absolute path to the build directory inside the tree means the archive
# only works on the machine that made it — which is the entire bug.
if grep -rIl "$work" "$root/lib/python3.11/site-packages" "$root/bin" 2>/dev/null | head -5 | grep -q .; then
  echo "!! build path is baked into the tree; it would not relocate" >&2
  grep -rIl "$work" "$root/lib/python3.11/site-packages" "$root/bin" 2>/dev/null | head -5 >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
archive="$OUT_DIR/marrow-runtime-${VERSION}-macos-arm64.tar.gz"
echo "==> $archive"
# `mlx` at the top so extraction lands on `runtime/mlx/bin/python`.
mv "$root" "$work/mlx"
# **COPYFILE_DISABLE, or the archive carries a shadow copy of itself.** macOS
# `tar` writes an AppleDouble `._name` member beside every file that has an
# extended attribute, to preserve it. bsdtar consumes those again on extract
# and never shows them, so `tar -tzf | wc -l` says 12,758 while the archive
# really holds 25,516 entries — and any extractor that is not bsdtar, this
# project's included, writes 12,758 junk files into the runtime tree.
COPYFILE_DISABLE=1 tar -czf "$archive" -C "$work" mlx

sha="$(shasum -a 256 "$archive" | cut -d' ' -f1)"
size="$(stat -f%z "$archive")"
unpacked="$(du -sk "$work/mlx" | cut -f1)"

cat <<REPORT

  Pin this in crates/model/src/runtime.rs:

      version:        "${VERSION}",
      sha256:         "${sha}",
      size:           ${size},
      unpacked_bytes: $((unpacked * 1024)),

  archive   $(echo "scale=1; $size/1048576" | bc) MiB
  unpacked  $(echo "scale=1; $unpacked/1024" | bc) MiB
REPORT
