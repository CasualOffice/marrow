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
        "thinkingTokens":0,"cachedPrefixTokens":0}
    <- {"id":"r2","event":"token","text":"Hel"}
    <- {"id":"r2","event":"done","promptTokens":812,"outputTokens":41,
        "thinkingTokens":0,"cachedPrefixTokens":0,"stopReason":"stop"}

    -> {"op":"embed","id":"r3","texts":["a","b"]}
    <- {"id":"r3","event":"embeddings","vectors":[[...],[...]]}

    -> {"op":"ping"} / {"op":"shutdown"}

Every failure is a `{"event":"error","code":...,"message":...}` line, never a
traceback on stdout: stdout is the protocol, stderr is the log.

Something that is worth telling the user but is *not* a failure — a setting
that could not be honoured, a cache that had to be dropped — is a
`{"id":...,"event":"warning","message":"...","code":...}` line, where `code` is
optional. It reaches the window as a notice beside the answer and does not end
the generation; the alternatives were an `error`, which throws a good answer
away, and silence. Rust accepts these already; nothing here emits one yet.
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
        # Embedding models are loaded by a different library and called a
        # different way; see `op_load`.
        self.kind = "generate"
        # Reused across requests that share a token prefix (LLM-040).
        #
        # `LRUPromptCache` rather than one cache and a trim, because **not every
        # model's cache can be trimmed**: Qwen 3.5 4B mixes `ArraysCache` with
        # `KVCache` and `can_trim_prompt_cache` returns False for it. A
        # trim-only design silently reuses nothing on exactly the model this is
        # for. The LRU keeps shorter prefixes as separate entries, so a
        # conversation reuses its preamble whether or not trimming works.
        #
        # Reuse stays prefix-*exact* either way (LLM-041): the trie matches on
        # the token sequence, so a fuzzy match cannot happen.
        self.cache = None
        self.cache_tokens = []

    # -- ops ---------------------------------------------------------------

    def op_ping(self, req):
        emit({"id": req.get("id"), "event": "pong", "protocol": PROTOCOL_VERSION})

    def op_load(self, req):
        path = req["model"]
        kind = req.get("kind", "generate")
        log(f"loading {path} as {kind}")

        if kind == "embedding":
            # **Not `mlx_lm`.** An embedding model is not a causal LM with the
            # head removed: EmbeddingGemma carries the SentenceTransformers
            # dense projection, and `mlx_lm.load` refuses it outright —
            # "Received 6 parameters not in model: dense.0.weight, ...".
            # Loading it as an LM and mean-pooling the base output would
            # *work*, in the sense of producing numbers, while discarding the
            # projection the model was trained to embed through.
            from mlx_embeddings import load as load_embedding

            self.model, self.tokenizer = load_embedding(path)
        else:
            # Imported here, not at module scope: a worker only ever asked to
            # `ping` should not pay a multi-second import, and an import
            # failure must arrive as a protocol error rather than a startup
            # crash.
            from mlx_lm import load

            self.model, self.tokenizer = load(path)

        self.kind = kind
        self.model_path = path
        self.cache = self._new_cache()
        self.cache_tokens = []

        info = {"id": req.get("id"), "event": "loaded", "model": path, "kind": kind}
        if kind == "embedding":
            # The width, measured rather than read from a config: it is what
            # every stored vector has to agree with, and a config that
            # disagrees with the weights would be discovered one query at a
            # time.
            info["dims"] = int(self._embed(["dimension probe"]).shape[1])
        else:
            # Whether this model's cache can be trimmed decides whether a
            # conversation reuses its preamble at all, so it is reported rather
            # than discovered later as "why is this slow".
            from mlx_lm.models.cache import can_trim_prompt_cache, make_prompt_cache

            probe = make_prompt_cache(self.model)
            info["cacheTrimmable"] = bool(can_trim_prompt_cache(probe))
            info["cacheKinds"] = sorted({type(c).__name__ for c in probe})
        emit(info)

    def op_unload(self, req):
        import mlx.core as mx

        self.model = None
        self.tokenizer = None
        self.cache = self._new_cache()
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
        cache, prompt, cached_prefix = self._prepare_cache(tokens)

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
        generated = []

        for chunk in stream_generate(
            self.model,
            self.tokenizer,
            prompt,
            max_tokens=max_tokens + thinking_budget,
            sampler=sampler,
            prompt_cache=cache,
        ):
            generated.append(chunk.token if hasattr(chunk, "token") else None)
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
        # Store what the cache actually holds: the whole prompt plus everything
        # generated. Recording only the prompt would describe a state the cache
        # is not in, and the next turn would continue from the wrong place.
        full = list(tokens) + [t for t in generated if t is not None]
        self.cache.insert_cache(self.model_path, full, cache)
        self.cache_tokens = full

    @staticmethod
    def _new_cache():
        from mlx_lm.models.cache import LRUPromptCache

        # LLM-044: a byte cap, not an entry count alone. Four entries is a
        # conversation's worth; 2 GB is a fraction of any model this machine
        # can hold, and the cache must never be the reason one stops fitting.
        return LRUPromptCache(max_size=4, max_bytes=2 << 30)

    def _prepare_cache(self, tokens):
        """Reuse as much of a previous prompt's KV cache as is exactly shared.

        Marrow's prompts share a large prefix by construction: the same system
        instructions, the same envelope framing, and the same documents carried
        across the turns of one conversation. Recomputing that every turn is the
        largest avoidable cost in the feature.

        Takes the fully-templated token ids, and returns
        `(cache, tokens_to_process, cached_prefix_tokens)`.
        """
        if self.cache is None:
            self.cache = self._new_cache()
        cache, suffix = self.cache.fetch_nearest_cache(self.model_path, list(tokens))
        if cache is None:
            from mlx_lm.models.cache import make_prompt_cache

            return make_prompt_cache(self.model), list(tokens), 0
        # The model needs at least one token to attend to; a zero-length suffix
        # has nothing to generate from.
        if not suffix:
            suffix = list(tokens[-1:])
        return cache, suffix, len(tokens) - len(suffix)

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
        if self.model is None:
            raise Failure("MOD_NOT_INSTALLED", "No model is loaded.")
        if self.kind != "embedding":
            raise Failure(
                "MOD_UNSUPPORTED_CAPABILITY",
                "The loaded model generates text; it does not produce embeddings. "
                "Load an embedding model first.",
            )
        texts = req["texts"]
        if not texts:
            emit({"id": req["id"], "event": "embeddings", "vectors": []})
            return
        vectors = self._embed(texts)
        emit({
            "id": req["id"],
            "event": "embeddings",
            "vectors": [[float(x) for x in row] for row in vectors],
        })

    # Longest sequence the embedder sees. Beyond this the tail is dropped
    # rather than the request failing: a chunk that is slightly too long should
    # still be findable, and Marrow's chunker already keeps them well under.
    MAX_EMBED_TOKENS = 512

    def _embed(self, texts):
        """Batch of texts to a `(n, dims)` array.

        **One at a time, and that is measured rather than cautious.** Padding a
        batch to its longest member changes the shorter members' vectors: with
        `mlx_embeddings` and EmbeddingGemma, the word "short" embedded beside a
        40-token passage agrees with itself embedded alone at only 0.89. The
        pooling masks padding correctly, so it is the attention that leaks —
        and a chunk that lands somewhere different depending on what happened
        to be batched with it makes the index disagree with the query for
        reasons nobody can see.

        Batching still happens, one level up: the caller sends many texts in
        one message, which is where the round-trip cost was. What it does not
        do is let them pad each other.

        (`mlx_embeddings.utils.generate` exists and does not work either — it
        calls the model with `input_ids=` and the model takes positional
        `inputs`.)
        """
        import mlx.core as mx

        rows = []
        for text in texts:
            enc = self.tokenizer.batch_encode_plus(
                [text],
                return_tensors="mlx",
                padding=False,
                truncation=True,
                max_length=Worker.MAX_EMBED_TOKENS,
            )
            out = self.model(enc["input_ids"], enc["attention_mask"])
            rows.append(out.text_embeds[0])
        return mx.stack(rows)


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
