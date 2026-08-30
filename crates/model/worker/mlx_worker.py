"""Marrow's MLX inference worker.

Runs as a **separate process** (LLM-020): an OOM here kills this and never the
index. Speaks JSON Lines on stdin/stdout — one request per line, one or more
responses per line — because a line-delimited protocol is trivially framed and
a partial line is obviously incomplete.

Nothing here decides *whether* work may run. The supervisor does that; this
process only does what it is told, and reports what happened.

Protocol
--------
    -> {"op":"load","model":"<path>","id":"r1"}
    <- {"id":"r1","event":"loaded","weightsBytes":3063000000}

    -> {"op":"generate","id":"r2","prompt":"...","maxTokens":512,
        "thinkingTokens":0,"cachePrefixTokens":0}
    <- {"id":"r2","event":"token","text":"Hel"}
    <- {"id":"r2","event":"done","promptTokens":812,"outputTokens":41,
        "thinkingTokens":0,"cachedPrefixTokens":0,"stopReason":"stop"}

    -> {"op":"embed","id":"r3","texts":["a","b"]}
    <- {"id":"r3","event":"embeddings","vectors":[[...],[...]]}

    -> {"op":"ping"} / {"op":"shutdown"}

Every failure is a `{"event":"error","code":...,"message":...}` line, never a
traceback on stdout: stdout is the protocol, stderr is the log.
"""

import json
import sys
import traceback

PROTOCOL_VERSION = 1


def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def log(msg):
    sys.stderr.write(f"[mlx_worker] {msg}\n")
    sys.stderr.flush()


class Worker:
    def __init__(self):
        self.model = None
        self.tokenizer = None
        self.model_path = None
        # Reused across requests that share a token prefix (LLM-040). Held
        # alongside the exact tokens it was built from, because reuse is
        # prefix-*exact*: a fuzzy match would continue from a state that never
        # produced this prompt, which is a wrong-answer generator rather than
        # an optimisation (LLM-041).
        self.cache = None
        self.cache_tokens = []

    # -- ops ---------------------------------------------------------------

    def op_ping(self, req):
        emit({"id": req.get("id"), "event": "pong", "protocol": PROTOCOL_VERSION})

    def op_load(self, req):
        # Imported here, not at module scope: a worker that is only ever asked
        # to `ping` should not pay a multi-second import, and an import failure
        # must arrive as a protocol error rather than a startup crash.
        from mlx_lm import load

        path = req["model"]
        log(f"loading {path}")
        self.model, self.tokenizer = load(path)
        self.model_path = path
        self.cache = None
        self.cache_tokens = []
        emit({"id": req.get("id"), "event": "loaded", "model": path})

    def op_unload(self, req):
        import mlx.core as mx

        self.model = None
        self.tokenizer = None
        self.cache = None
        self.cache_tokens = []
        # LLM-049: the cache goes with the weights. A model unloaded while its
        # cache stays resident has not been unloaded.
        mx.clear_cache()
        emit({"id": req.get("id"), "event": "unloaded"})

    def op_generate(self, req):
        from mlx_lm import stream_generate
        from mlx_lm.sample_utils import make_sampler

        if self.model is None:
            raise Failure("MOD_NOT_INSTALLED", "No model is loaded.")

        max_tokens = int(req.get("maxTokens", 512))
        thinking_budget = int(req.get("thinkingTokens", 0))
        thinking_on = thinking_budget > 0

        tokens = self._as_chat_turn(req["prompt"], thinking_on)
        prompt, cached_prefix = self._prepare_cache(tokens)

        # Thorough is not a different code path — it is a larger budget and a
        # temperature that lets the model explore before it commits.
        sampler = make_sampler(temp=0.7 if thinking_on else 0.0)

        # The whole prompt, not just the part that had to be prefilled. mlx
        # reports the latter, and "120 prompt tokens, 175 cached" is a sentence
        # nobody can read.
        prompt_tokens = len(tokens)
        output_tokens = 0
        thinking_tokens = 0
        stop_reason = "length"

        # Reasoning arrives inside <think>...</think>. Split at the wire rather
        # than in the UI: the UI must never have to guess which half of a
        # stream it is rendering (GEN-014), and thinking is never citable
        # evidence (GEN-015).
        in_thinking = False
        pending = ""
        answer = []
        thoughts = []

        for chunk in stream_generate(
            self.model,
            self.tokenizer,
            prompt,
            max_tokens=max_tokens + thinking_budget,
            sampler=sampler,
            prompt_cache=self.cache,
        ):
            if getattr(chunk, "finish_reason", None):
                stop_reason = chunk.finish_reason

            pending += chunk.text
            # Hold back anything that could be a partial tag, so "<thi" is
            # never emitted as answer text and then retracted.
            while pending:
                if not in_thinking:
                    cut = pending.find("<think>")
                    if cut == -1:
                        keep = self._safe_prefix(pending, "<think>")
                        if keep:
                            answer.append(keep)
                            emit({"id": req["id"], "event": "token",
                                  "channel": "text", "text": keep})
                            output_tokens += 1
                            pending = pending[len(keep):]
                        break
                    if cut:
                        answer.append(pending[:cut])
                        emit({"id": req["id"], "event": "token",
                              "channel": "text", "text": pending[:cut]})
                        output_tokens += 1
                    pending = pending[cut + len("<think>"):]
                    in_thinking = True
                else:
                    cut = pending.find("</think>")
                    if cut == -1:
                        keep = self._safe_prefix(pending, "</think>")
                        if keep:
                            thoughts.append(keep)
                            emit({"id": req["id"], "event": "token",
                                  "channel": "thinking", "text": keep})
                            thinking_tokens += 1
                            pending = pending[len(keep):]
                        break
                    if cut:
                        thoughts.append(pending[:cut])
                        emit({"id": req["id"], "event": "token",
                              "channel": "thinking", "text": pending[:cut]})
                        thinking_tokens += 1
                    pending = pending[cut + len("</think>"):]
                    in_thinking = False

        if pending:
            target = thoughts if in_thinking else answer
            target.append(pending)
            emit({"id": req["id"], "event": "token",
                  "channel": "thinking" if in_thinking else "text", "text": pending})

        emit({
            "id": req["id"],
            "event": "done",
            "promptTokens": prompt_tokens,
            "outputTokens": output_tokens,
            "thinkingTokens": thinking_tokens,
            "cachedPrefixTokens": cached_prefix,
            "stopReason": stop_reason,
            "thinking": "".join(thoughts) or None,
        })
        # The cache now holds the prompt plus everything generated. Record
        # exactly that, or the next request's prefix comparison is against a
        # state the cache is not in.
        self.cache_tokens = self.cache_tokens + list(prompt)

    def _prepare_cache(self, tokens):
        """Reuse as much of the previous prompt's KV cache as is exactly shared.

        Marrow's prompts share a large prefix by construction: the same system
        instructions, the same envelope framing, and often the same documents
        across a follow-up question. Recomputing that every turn is the single
        largest avoidable cost in the feature.

        Takes the fully-templated token ids, and returns
        `(tokens_to_process, cached_prefix_tokens)`.
        """
        from mlx_lm.models.cache import (
            can_trim_prompt_cache,
            make_prompt_cache,
            trim_prompt_cache,
        )

        if self.cache is None:
            self.cache = make_prompt_cache(self.model)
            self.cache_tokens = []
            return tokens, 0

        # Prefix-exact. One differing token and the rest of the cache describes
        # a different conversation.
        shared = 0
        for a, b in zip(self.cache_tokens, tokens):
            if a != b:
                break
            shared += 1

        # Never reuse the whole prompt: the model needs at least one token to
        # attend to, and a zero-length suffix has nothing to generate from.
        shared = min(shared, len(tokens) - 1)

        if shared <= 0 or not can_trim_prompt_cache(self.cache):
            self.cache = make_prompt_cache(self.model)
            self.cache_tokens = []
            return tokens, 0

        drop = len(self.cache_tokens) - shared
        if drop > 0:
            trimmed = trim_prompt_cache(self.cache, drop)
            if trimmed != drop:
                # The cache could not be trimmed to the shared prefix, so what
                # is in it no longer matches what we think is in it. Start
                # again rather than continue from a state we cannot describe.
                self.cache = make_prompt_cache(self.model)
                self.cache_tokens = []
                return tokens, 0
        self.cache_tokens = self.cache_tokens[:shared]
        return tokens[shared:], shared

    def _as_chat_turn(self, envelope, thinking_on):
        """Wrap the envelope in the model's own turn structure, as token ids.

        Without this an instruct model *continues* the prompt instead of
        answering it — it autocompletes the envelope, delimiters and all.

        The whole envelope goes in as one user turn. Its internal SYS/EVIDENCE
        blocks keep their meaning: the labelling that makes retrieved text
        inert is Marrow's delimiters, not the chat roles.

        Token ids rather than text, because the KV cache is keyed on the exact
        token sequence and re-encoding a rendered template is a second chance
        to disagree with the first.
        """
        apply = getattr(self.tokenizer, "apply_chat_template", None)
        if apply is None or getattr(self.tokenizer, "chat_template", None) is None:
            # A base model with no template. The caller gets continuation
            # behaviour, which is correct for a base model.
            return self.tokenizer.encode(envelope)
        messages = [{"role": "user", "content": envelope}]
        try:
            # Qwen3 and friends gate reasoning on this flag, which is exactly
            # Marrow's Fast/Thorough switch reaching the model.
            out = apply(messages, add_generation_prompt=True,
                        enable_thinking=thinking_on)
        except TypeError:
            # Templates that do not know the flag.
            out = apply(messages, add_generation_prompt=True)
        # `apply_chat_template` returns ids when it tokenizes and a string when
        # it does not; both are in the wild.
        return self.tokenizer.encode(out) if isinstance(out, str) else list(out)

    @staticmethod
    def _safe_prefix(text, tag):
        """The part of `text` that cannot be the start of `tag`.

        Emitting "<thi" as answer text and retracting it later would make the
        stream lie for a few frames.
        """
        for n in range(min(len(tag) - 1, len(text)), 0, -1):
            if text.endswith(tag[:n]):
                return text[:-n]
        return text

    def op_embed(self, req):
        import mlx.core as mx

        if self.model is None:
            raise Failure("MOD_NOT_INSTALLED", "No model is loaded.")
        texts = req["texts"]
        vectors = []
        for t in texts:
            ids = mx.array([self.tokenizer.encode(t)])
            out = self.model(ids)
            # Mean-pool the last hidden state. Deterministic, and the same
            # pooling must be used at index time and at query time or the
            # vectors are not comparable.
            vec = out[0].mean(axis=0)
            vectors.append([float(x) for x in vec])
        emit({"id": req["id"], "event": "embeddings", "vectors": vectors})


class Failure(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


OPS = {
    "ping": Worker.op_ping,
    "load": Worker.op_load,
    "unload": Worker.op_unload,
    "generate": Worker.op_generate,
    "embed": Worker.op_embed,
}


def main():
    worker = Worker()
    emit({"event": "ready", "protocol": PROTOCOL_VERSION})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            emit({"event": "error", "code": "INT_INVARIANT_VIOLATED",
                  "message": f"The worker received a line that was not JSON: {e}"})
            continue

        op = req.get("op")
        if op == "shutdown":
            emit({"id": req.get("id"), "event": "goodbye"})
            return
        handler = OPS.get(op)
        if handler is None:
            emit({"id": req.get("id"), "event": "error", "code": "CFG_INVALID",
                  "message": f"The worker does not know the operation {op!r}."})
            continue
        try:
            handler(worker, req)
        except Failure as f:
            emit({"id": req.get("id"), "event": "error",
                  "code": f.code, "message": f.message})
        except MemoryError:
            # Reported rather than crashed, so the supervisor sees a reason
            # instead of an exit code (LLM-023).
            emit({"id": req.get("id"), "event": "error",
                  "code": "MOD_INSUFFICIENT_MEMORY",
                  "message": "The model ran out of memory. Try a smaller model "
                             "or a shorter context."})
        except Exception as e:
            log(traceback.format_exc())
            emit({"id": req.get("id"), "event": "error", "code": "MOD_WORKER_CRASH",
                  "message": f"{type(e).__name__}: {e}"})


if __name__ == "__main__":
    main()
