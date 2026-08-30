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
        # Reused across requests that share a prompt prefix (LLM-040). Keyed by
        # the prefix hash the supervisor computed, so the cache identity is
        # decided in one place rather than two.
        self.cache = None
        self.cache_key = None

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
        self.cache_key = None
        emit({"id": req.get("id"), "event": "loaded", "model": path})

    def op_unload(self, req):
        import mlx.core as mx

        self.model = None
        self.tokenizer = None
        self.cache = None
        self.cache_key = None
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

        prompt = self._as_chat_turn(req["prompt"], thinking_on)

        # Thorough is not a different code path — it is a larger budget and a
        # temperature that lets the model explore before it commits.
        sampler = make_sampler(temp=0.7 if thinking_on else 0.0)

        prompt_tokens = 0
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
        ):
            prompt_tokens = getattr(chunk, "prompt_tokens", prompt_tokens) or prompt_tokens
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
            "cachedPrefixTokens": 0,
            "stopReason": stop_reason,
            "thinking": "".join(thoughts) or None,
        })

    def _as_chat_turn(self, envelope, thinking_on):
        """Wrap the envelope in the model's own turn structure.

        Without this an instruct model *continues* the prompt instead of
        answering it — it autocompletes the envelope, delimiters and all.

        The whole envelope goes in as one user turn. Its internal SYS/EVIDENCE
        blocks keep their meaning: the labelling that makes retrieved text
        inert is Marrow's delimiters, not the chat roles.
        """
        tok = getattr(self.tokenizer, "apply_chat_template", None)
        if tok is None or getattr(self.tokenizer, "chat_template", None) is None:
            # A base model with no template. Returned as-is, and the caller
            # gets continuation behaviour — which is correct for a base model.
            return envelope
        messages = [{"role": "user", "content": envelope}]
        try:
            # Qwen3 and friends gate reasoning on this flag, which is exactly
            # Marrow's Fast/Thorough switch reaching the model.
            return tok(messages, add_generation_prompt=True,
                       enable_thinking=thinking_on)
        except TypeError:
            # Templates that do not know the flag.
            return tok(messages, add_generation_prompt=True)

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
