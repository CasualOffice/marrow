/**
 * Development fixtures — **dev only**.
 *
 * `api.ts` reaches for this file only when `import.meta.env.DEV` is true *and*
 * the Tauri IPC bridge is absent, i.e. when the UI is opened in a plain browser
 * via `pnpm dev`. The branch is statically false in a production build, so
 * Rollup drops both the branch and this module: nothing here ships.
 *
 * The shapes mirror `crates/desktop/src/commands.rs` exactly. When a command
 * changes shape this file has to change with it — a fixture returning last
 * month's shape is a UI that passes in dev and breaks in the app, which is
 * what happened to `read_region` when it grew `firstLine`/`truncated`.
 */

import type {
  AskEvent,
  Citation,
  ConversationDetail,
  ConversationSummary,
  NewTurn,
  SavedTurn,
  StoredTurn,
  FileDetail,
  FileRow,
  IndexHealth,
  Region,
  SearchHit,
  ModelRow,
  ModelsSnapshot,
  SearchResponse,
  WorkspaceRow,
} from "../api";


/**
 * Models. Shaped to exercise the three cases that must look different:
 * installed and running, offerable but blocked on a missing digest, and too
 * large for this machine. A fixture where every row looks the same hides
 * exactly the bug this page exists to prevent.
 */
let devProfile = "balanced";

const DEV_MODELS: readonly ModelRow[] = [
  {
    id: "ollama:qwen2.5:7b",
    displayName: "qwen2.5:7b",
    family: "qwen2",
    paramsB: 7.6,
    quantization: "Q4",
    format: "gguf",
    contextLimit: 8192,
    role: "Already installed in Ollama.",
    source: "detected",
    detectedIn: "ollama",
    installed: true,
    downloadable: false,
    blockedReason: null,
    repo: null,
    revisionShort: null,
    fileCount: 0,
    downloadBytes: 0,
    runContext: 4096,
    kvMeasured: false,
    progress: null,
    licence: "Set by whoever installed it",
    licenceUrl: null,
    commercialUse: null,
    capabilities: ["structured output"],
    reasoningUnavailable: "qwen2.5:7b answers directly.",
    fit: "comfortable",
    fitReason: "Needs about 8.0 GB, and 9.1 GB is free.",
    breakdown:
      "weights 4.6 GB · KV cache 655 MB · runtime 0 MB · embedding model 200 MB · OS reserve 2.5 GB",
    requiredBytes: 7_955_000_000,
    state: { state: "installed" },
    consecutiveFailures: 0,
    suspendedReason: null,
  },
  {
    id: "qwen3.5-4b-mlx-q4",
    displayName: "Qwen 3.5 4B",
    family: "qwen",
    paramsB: 4.0,
    quantization: "Q4",
    format: "mlx",
    contextLimit: 262144,
    role: "Primary candidate. Routes the question and writes the answer.",
    source: "catalogue",
    detectedIn: null,
    installed: false,
    downloadable: true,
    blockedReason: null,
    repo: "mlx-community/Qwen3.5-4B-MLX-4bit",
    revisionShort: "32f3e8ecf654",
    fileCount: 10,
    downloadBytes: 3061129077,
    runContext: 8192,
    kvMeasured: true,
    progress: null,
    licence: "Apache-2.0",
    licenceUrl: "https://www.apache.org/licenses/LICENSE-2.0",
    commercialUse: true,
    capabilities: ["tools", "structured output", "reasoning", "multilingual"],
    reasoningUnavailable: null,
    fit: "comfortable",
    fitReason: "Needs about 7.2 GB, and 9.1 GB is free.",
    breakdown:
      "weights 3.1 GB · KV cache 1.1 GB · runtime 350 MB · embedding model 200 MB · OS reserve 2.5 GB",
    requiredBytes: 7184870901,
    state: { state: "absent" },
    consecutiveFailures: 0,
    suspendedReason: null,
  },
  {
    id: "nemotron-3-nano-4b-mlx-q4",
    displayName: "Nemotron 3 Nano 4B",
    family: "nemotron",
    paramsB: 4.0,
    quantization: "Q4",
    format: "mlx",
    contextLimit: 262144,
    role: "Reasoning and agent behaviour — the Thorough-mode comparison.",
    source: "catalogue",
    detectedIn: null,
    installed: false,
    downloadable: true,
    blockedReason: null,
    repo: "mlx-community/NVIDIA-Nemotron-3-Nano-4B-4bit",
    revisionShort: "c4d79ba1901d",
    fileCount: 11,
    downloadBytes: 2254291874,
    runContext: 8192,
    kvMeasured: true,
    progress: {
      modelId: "nemotron-3-nano-4b-mlx-q4",
      stage: { stage: "downloading", file: "model.safetensors", index: 2, of: 11 },
      bytesDone: 834087993,
      bytesTotal: 2254291874,
      bytesPerSec: 11_500_000,
      etaSecs: 123,
    },
    licence: "NVIDIA Open Model Licence",
    licenceUrl: null,
    commercialUse: null,
    capabilities: ["tools", "structured output", "reasoning"],
    reasoningUnavailable: null,
    fit: "comfortable",
    fitReason: "Needs about 6.7 GB, and 9.1 GB is free.",
    breakdown:
      "weights 2.3 GB · KV cache 1.4 GB · runtime 350 MB · embedding model 200 MB · OS reserve 2.5 GB",
    requiredBytes: 6713578018,
    state: { state: "absent" },
    consecutiveFailures: 0,
    suspendedReason: null,
  },
  {
    id: "granite-4.1-3b-mlx-q4",
    displayName: "Granite 4.1 3B",
    family: "granite",
    paramsB: 3.0,
    quantization: "Q4",
    format: "mlx",
    contextLimit: 131072,
    role: "Tool calling and structured output — the MCP-facing workload.",
    source: "catalogue",
    detectedIn: null,
    installed: false,
    downloadable: true,
    blockedReason: null,
    repo: "mlx-community/granite-4.1-3b-4bit",
    revisionShort: "b1b476b5a17c",
    fileCount: 7,
    downloadBytes: 2134388914,
    runContext: 8192,
    kvMeasured: true,
    progress: null,
    licence: "Apache-2.0",
    licenceUrl: "https://www.apache.org/licenses/LICENSE-2.0",
    commercialUse: true,
    capabilities: ["tools", "structured output"],
    reasoningUnavailable: "Granite 4.1 3B answers directly.",
    fit: "comfortable",
    fitReason: "Needs about 5.9 GB, and 9.1 GB is free.",
    breakdown:
      "weights 2.1 GB · KV cache 671 MB · runtime 350 MB · embedding model 200 MB · OS reserve 2.5 GB",
    requiredBytes: 5855477554,
    state: { state: "absent" },
    consecutiveFailures: 0,
    suspendedReason: null,
  },
  {
    id: "qwen3-0.6b-mlx-q4",
    displayName: "Qwen 3 0.6B",
    family: "qwen",
    paramsB: 0.6,
    quantization: "Q4",
    format: "mlx",
    contextLimit: 40960,
    role: "The router. Intent, query rewrite and NER — runs once per file, so it must be cheap.",
    source: "catalogue",
    detectedIn: null,
    installed: false,
    downloadable: true,
    blockedReason: null,
    repo: "mlx-community/Qwen3-0.6B-4bit",
    revisionShort: "73e3e38d9813",
    fileCount: 9,
    downloadBytes: 351383618,
    runContext: 4096,
    kvMeasured: true,
    progress: null,
    licence: "Apache-2.0",
    licenceUrl: "https://www.apache.org/licenses/LICENSE-2.0",
    commercialUse: true,
    capabilities: ["structured output", "multilingual"],
    reasoningUnavailable: "Qwen 3 0.6B answers directly.",
    fit: "comfortable",
    fitReason: "Needs about 3.9 GB, and 9.1 GB is free.",
    breakdown:
      "weights 351 MB · KV cache 470 MB · runtime 350 MB · embedding model 200 MB · OS reserve 2.5 GB",
    requiredBytes: 3871145666,
    state: { state: "absent" },
    consecutiveFailures: 0,
    suspendedReason: null,
  },
  {
    id: "embeddinggemma-300m-mlx-q4",
    displayName: "EmbeddingGemma 300M",
    family: "gemma",
    paramsB: 0.3,
    quantization: "Q4",
    format: "mlx",
    contextLimit: 2048,
    role: "The embedder. Semantic search runs on this, so it stays loaded.",
    source: "catalogue",
    detectedIn: null,
    installed: false,
    downloadable: true,
    blockedReason: null,
    repo: "mlx-community/embeddinggemma-300m-4bit",
    revisionShort: "5d9ef074df39",
    fileCount: 12,
    downloadBytes: 212491172,
    runContext: 2048,
    kvMeasured: true,
    progress: null,
    licence: "Gemma Terms of Use",
    licenceUrl: "https://ai.google.dev/gemma/terms",
    commercialUse: null,
    capabilities: ["multilingual", "embedding"],
    reasoningUnavailable: null,
    fit: "comfortable",
    fitReason: "Needs about 3.1 GB, and 9.1 GB is free.",
    breakdown:
      "weights 212 MB · KV cache 50 MB · runtime 350 MB · embedding model 0 MB · OS reserve 2.5 GB",
    requiredBytes: 3112822820,
    state: { state: "absent" },
    consecutiveFailures: 0,
    suspendedReason: null,
  },
];

function devModels(): ModelsSnapshot {
  return {
    machine: "17 GB unified · 10 cores · Mac16,12",
    tierHeadline: "Comfortable up to about 8B at 4-bit.",
    unifiedMemory: true,
    totalBytes: 17_179_869_184,
    availableBytes: 9_100_000_000,
    sustainedLoad: 0.31,
    thermal: "unknown",
    sampleStale: false,
    // Part-built on purpose: the state worth looking at in dev is the one
    // where coverage is neither 0 nor 100.
    semantic: {
      ready: true,
      embedded: 12_400,
      remaining: 42_287,
      failed: 3,
      running: false,
      problem: null,
      model: "EmbeddingGemma 300M",
    },
    residentBytes: 0,
    modelsDirProblem: null,
    detected: [{ runtime: "Ollama", port: 11434, modelCount: 1 }],
    detectionProblems: [],
    profiles: [
      {
        id: "efficient",
        label: "Efficient",
        detail: "Lowest memory and battery use. About 2B, local.",
        generatorParamsB: 2,
        selected: devProfile === "efficient",
        available: true,
        unavailableReason: null,
      },
      {
        id: "balanced",
        label: "Balanced",
        detail: "About 4B, local. Recommended.",
        generatorParamsB: 4,
        selected: devProfile === "balanced",
        available: true,
        unavailableReason: null,
      },
      {
        id: "larger_local",
        label: "Larger local model",
        detail: "8B and above where it fits. More memory, slower to answer.",
        generatorParamsB: 8,
        selected: devProfile === "larger_local",
        available: true,
        unavailableReason: null,
      },
      {
        id: "cloud",
        label: "Cloud",
        detail: "A frontier model over the network. Content leaves this device.",
        generatorParamsB: 0,
        selected: devProfile === "cloud",
        available: true,
        unavailableReason: null,
      },
    ],
    router: {
      workload: "routing",
      paramsB: 1.5,
      resident: true,
      why: "Classification, query rewrite and NER. Runs once per file, so it must be cheap.",
    },
    generator: {
      workload: "generation",
      paramsB: 4,
      resident: false,
      why: "The quality knee for grounded answering. Loaded on demand, unloaded when idle.",
    },
    embedder: {
      workload: "embedding",
      paramsB: 0.1,
      resident: true,
      why: "Search is the product; the embedder does not go cold because generation did.",
    },
    models: DEV_MODELS,
    runtimeReady: true,
    runtimeSetup: null,
    runtimeStatus:
      "MLX is available on this machine. A model that is installed and fits can answer questions locally — nothing leaves this device.",
  };
}

const DAY = 86_400_000;
const now = Date.now();

const SEEDS: ReadonlyArray<Omit<SearchHit, "rank" | "location" | "path">> = [
  {
    relativePath: "services/vault/src/auth/token.rs",
    line: 142,
    breadcrumb: "impl TokenService › fn refresh_token",
    excerpt:
      "    pub async fn refresh_token(&self, ctx: &Ctx) -> Result<Token> {\n        let claims = self.decode(&ctx.refresh)?;",
    provenance: "exact",
    reason: "exact",
    citable: true,
    modifiedMs: now - 2 * DAY,
    fileId: "01M17KJXJ2K51K4824XQ56H2Q7",
  },
  {
    relativePath: "docs/auth-design.md",
    line: 88,
    breadcrumb: "Authentication › Refresh token rotation",
    excerpt:
      "## Refresh token rotation\nTokens rotate on each use; the previous token is revoked after a grace period.",
    provenance: "exact",
    reason: "semantic",
    citable: true,
    modifiedMs: now - 21 * DAY,
    fileId: "01M17KJXJ2K51K4824XQ56H2Q8",
  },
  {
    relativePath: "notes/2026-06-standup.md",
    line: 12,
    breadcrumb: "§ June standup",
    excerpt: "decided to move refresh tokens out of localStorage",
    provenance: "degraded",
    reason: "semantic",
    citable: true,
    modifiedMs: now - 56 * DAY,
    fileId: "01M17KJXJ2K51K4824XQ56H2Q9",
  },
  {
    /*
     * The row that made the user write "its confusing": it matched on the
     * *path*, so its excerpt is the top of the file and does not contain the
     * query. The backend centres content matches on the term now, which cannot
     * help a path match — so the row has to say that itself.
     */
    relativePath: "services/vault/migrations/0014_refresh_session.sql",
    line: null,
    breadcrumb: "migrations › 0014_refresh_session",
    excerpt: "BEGIN;\n-- session storage for the vault service",
    provenance: "exact",
    reason: "path",
    citable: true,
    modifiedMs: now - 35 * DAY,
    fileId: "01M17KJXJ2K51K4824XQ56H2QA",
  },
  {
    relativePath: "notes/agent/2026-06-refactor-plan.md",
    line: 4,
    breadcrumb: "§ Plan › Token service",
    excerpt: "The refresh token path should move behind a trait boundary.",
    provenance: "approximate",
    reason: "semantic",
    citable: false,
    modifiedMs: now - 4 * DAY,
    fileId: "01M17KJXJ2K51K4824XQ56H2QB",
  },
];

const ROOT = "/Users/dev/melp";

function hits(query: string): SearchHit[] {
  const q = query.toLowerCase();
  const matched = SEEDS.filter(
    (s) =>
      q
        .split(/\s+/)
        .filter(Boolean)
        .some(
          (w) =>
            s.relativePath.toLowerCase().includes(w) ||
            s.excerpt.toLowerCase().includes(w) ||
            s.breadcrumb.toLowerCase().includes(w),
        ) || q === "auth refresh token",
  );
  // Enough rows to exercise the virtualizer.
  const many: SearchHit[] = [];
  for (let i = 0; i < (matched.length > 0 ? 4 : 0); i += 1) {
    for (const s of matched) {
      const suffix = i === 0 ? "" : `.${i}`;
      many.push({
        ...s,
        rank: many.length + 1,
        path: `${ROOT}/${s.relativePath}${suffix}`,
        relativePath: `${s.relativePath}${suffix}`,
        location:
          s.line === null
            ? `${s.relativePath}${suffix}`
            : `${s.relativePath}${suffix}:${s.line}`,
        fileId: `${s.fileId}${suffix}`,
      });
    }
  }
  return many;
}

/*
 * Four workspaces, each in a different state, because the sidebar and Status
 * have to make them all legible at once (GUI §11):
 *   melp      a few files a parser could not finish — the actionable one
 *   pictures  a third of it has no parser, and that is *fine*
 *   drafts    read but not yet parsed, plus cloud-only files it never opened
 *   icloud    registered and completely empty
 *
 * `pictures` is the fixture the split exists for: 1,207 files with no
 * searchable text, none of it wrong, and the card must render healthy. Before
 * the split it was the same warning triangle as `melp`.
 *
 * In every row `noParser + parseFailed + notProcessed === unindexed`, as it is
 * in `catalog.rs`. A fixture where the parts do not add up would hide exactly
 * the bug the arithmetic is there to prevent.
 */
const WORKSPACES: WorkspaceRow[] = [
  {
    name: "melp",
    path: "/Users/dev/melp",
    files: 9435,
    chunks: 48_210,
    contentBytes: 1_180_000_000,
    cloudOnly: 0,
    unindexed: 34,
    noParser: 26,
    parseFailed: 8,
    notProcessed: 0,
  },
  {
    name: "pictures",
    path: "/Users/dev/Pictures",
    files: 3478,
    chunks: 11_895,
    contentBytes: 214_000_000,
    cloudOnly: 0,
    unindexed: 1207,
    noParser: 1207,
    parseFailed: 0,
    notProcessed: 0,
  },
  {
    name: "drafts",
    path: "/Users/dev/Drafts",
    files: 2140,
    chunks: 6_400,
    contentBytes: 88_000_000,
    cloudOnly: 412,
    unindexed: 619,
    noParser: 90,
    parseFailed: 0,
    notProcessed: 529,
  },
  {
    name: "icloud",
    path: "/Users/dev/Library/Mobile Documents",
    files: 0,
    chunks: 0,
    contentBytes: 0,
    cloudOnly: 0,
    unindexed: 0,
    noParser: 0,
    parseFailed: 0,
    notProcessed: 0,
  },
];

// The totals are the sum of the workspaces above, because the footer and the
// cards are the same fact about the same index and a fixture where they
// disagree is a fixture that hides the day they really do.
const HEALTH: IndexHealth = {
  files: 15_053,
  chunks: 66_505,
  contentBytes: 1_482_000_000,
  cloudOnly: 412,
  schemaVersion: 7,
  // Stale on purpose: the state worth looking at in dev is the one the banner
  // exists for.
  lastIndexedMs: Date.now() - 9 * 3600 * 1000,
  watcher: "unavailable",
  mayBeStale: true,
};

/* A few hundred rows, so the Files browser is browsing rather than listing. */
const FILE_SEEDS: ReadonlyArray<[string, string, number, boolean]> = [
  ["melp", "services/vault/src/auth/token.rs", 18_400, false],
  ["melp", "services/vault/src/auth/session.rs", 9_100, false],
  ["melp", "services/vault/src/lib.rs", 2_300, false],
  ["melp", "services/vault/migrations/0014_refresh_session.sql", 1_180, false],
  ["melp", "docs/auth-design.md", 24_800, false],
  ["melp", "docs/LLD.md", 91_000, false],
  ["melp", "notes/2026-06-standup.md", 3_400, false],
  ["melp", "notes/agent/2026-06-refactor-plan.md", 5_900, false],
  ["melp", "design/Main.dc.html", 140_000, false],
  ["melp", "target/debug/marrow", 41_000_000, true],
  ["pictures", "2026/06/IMG_4821.heic", 3_900_000, true],
  ["pictures", "2026/06/IMG_4822.heic", 4_100_000, true],
  ["pictures", "2026/05/scan-contract.pdf", 880_000, false],
  ["pictures", "screenshots/2026-06-14 at 09.41.png", 1_240_000, true],
  ["pictures", "raw/DSC_0091.arw", 24_000_000, true],
];

const FILES: FileRow[] = (() => {
  const out: FileRow[] = [];
  for (let i = 0; i < 24; i += 1) {
    for (const [workspace, rel, size, metaOnly] of FILE_SEEDS) {
      const suffix = i === 0 ? "" : `.${i}`;
      const root =
        workspace === "melp" ? ROOT : "/Users/dev/Pictures";
      const relativePath = `${rel}${suffix}`;
      out.push({
        workspace,
        path: `${root}/${relativePath}`,
        relativePath,
        sizeBytes: size,
        modifiedMs: now - (out.length * 37 + 40) * 60_000,
        chunks: metaOnly ? 0 : Math.max(1, Math.round(size / 1400)),
        metadataOnly: metaOnly,
      });
    }
  }
  // Newest first, as the command promises.
  return out.sort((a, b) => (b.modifiedMs ?? 0) - (a.modifiedMs ?? 0));
})();

const SOURCE = [
  "    /// Rotate a refresh token, revoking the previous one.",
  "    ///",
  "    /// Returns `Stale` if the presented token was already used.",
  "    #[tracing::instrument(skip(self, ctx))]",
  "    pub async fn refresh_token(&self, ctx: &Ctx) -> Result<Token> {",
  "        let claims = self.decode(&ctx.refresh)?;",
  "        if self.revoked.contains(&claims.jti) {",
  '            return Err(Error::new(Code::PolDenied, "token already used"));',
  "        }",
  "        let next = self.mint(claims.sub, ctx.now)?;",
  "        self.revoked.insert(claims.jti);",
  "        Ok(next)",
  "    }",
];

/**
 * Conversations, in memory for the life of the page.
 *
 * Enough to exercise the list, the ordering, renaming and deleting in
 * `pnpm dev`, and nothing more: persistence is the Rust half and a fixture that
 * simulated it would be testing the fixture.
 */
const DEV_THREADS = new Map<
  string,
  {
    title: string;
    scope: string | null;
    updatedMs: number;
    turns: StoredTurn[];
  }
>([
  [
    "01DEVSEED",
    {
      title: "How does the vault rotate tokens?",
      scope: null,
      updatedMs: now - 2 * 3_600_000,
      turns: [
        {
          question: "How does the vault rotate tokens?",
          answer:
            "Every fifteen minutes; a refresh that arrives after the window is rejected rather than extended [E1].",
          thorough: false,
          model: "Qwen 3.5 4B",
          scope: null,
          citations: [
            {
              id: "E1",
              path: `${ROOT}/services/vault/src/auth/token.rs`,
              relativePath: "services/vault/src/auth/token.rs",
              location: "services/vault/src/auth/token.rs:88",
              line: 88,
              excerpt: "Tokens are rotated every fifteen minutes.",
              provenance: "exact",
            },
          ],
          excluded: [],
          projects: ["services/vault"],
          usage: {
            promptTokens: 812,
            outputTokens: 44,
            thinkingTokens: 0,
            cachedPrefixTokens: 0,
            stopReason: "stop",
            elapsedMs: 2_100,
          },
          askedMs: now - 2 * 3_600_000,
        },
      ],
    },
  ],
]);

function devConversations(): ConversationSummary[] {
  return [...DEV_THREADS.entries()]
    .map(([id, t]) => ({
      id,
      title: t.title,
      scope: t.scope,
      createdMs: t.updatedMs,
      updatedMs: t.updatedMs,
      turns: t.turns.length,
    }))
    .sort((a, b) => b.updatedMs - a.updatedMs);
}

export async function mockInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  await new Promise((r) => setTimeout(r, 6));
  switch (cmd) {
    case "search": {
      const query = String(args?.["query"] ?? "");
      const h = hits(query);
      const res: SearchResponse = {
        query,
        total: h.length,
        // Deliberately larger than `total`: the footer must read this one, and
        // a fixture where the two agree would hide it if the footer regressed.
        matched: h.length === 0 ? 0 : 842,
        elapsedMs: 8,
        hits: h,
        branches: ["lexical"],
      };
      return res as T;
    }
    case "list_workspaces":
      return WORKSPACES as T;
    case "index_health":
      return HEALTH as T;
    case "list_files": {
      const workspace = args?.["workspace"];
      const prefix = args?.["prefix"];
      const limit = Number(args?.["limit"] ?? 500);
      let rows = FILES;
      if (typeof workspace === "string" && workspace !== "")
        rows = rows.filter((f) => f.workspace === workspace);
      if (typeof prefix === "string" && prefix.trim() !== "") {
        const p = prefix.trim().toLowerCase();
        rows = rows.filter((f) => f.relativePath.toLowerCase().includes(p));
      }
      return rows.slice(0, limit) as T;
    }
    case "file_detail": {
      const path = String(args?.["path"] ?? "");
      const row = FILES.find((f) => f.path === path);
      const d: FileDetail = {
        path,
        fileId: "01M17KJXJ2K51K4824XQ56H2Q7",
        workspace: row?.workspace ?? "melp",
        sizeBytes: row?.sizeBytes ?? 14_100_000,
        contentHash: "blake3:9f2a1c8e4d",
        mime: "text/x-rust",
        modifiedMs: row?.modifiedMs ?? now - 2 * DAY,
        versions: 3,
        chunks: row?.chunks ?? 18,
        tierState: row?.metadataOnly === true ? "cloud-only" : "resident",
        citable: !path.includes("/agent/"),
        previousPaths: [`${ROOT}/services/vault/src/token.rs`],
        embeddedMetadata: null,
        structure: null,
      };
      return d as T;
    }
    case "read_region": {
      // `{ firstLine, lines, truncated }` since the core stopped making the UI
      // reconstruct the first line number from a duplicated constant.
      const around = args?.["aroundLine"];
      if (typeof around !== "number") {
        const r: Region = { firstLine: 1, lines: SOURCE, truncated: false };
        return r as T;
      }
      const first = Math.max(1, around - 4);
      const r: Region = { firstLine: first, lines: SOURCE, truncated: true };
      return r as T;
    }
    case "models_overview":
    case "refresh_model_detection":
      return devModels() as T;
    case "download_model":
    case "cancel_model_download":
    case "dismiss_model_download":
      // The transfer itself is Rust; dev mode shows the bar the fixture
      // already carries rather than simulating a network.
      return devModels() as T;
    case "set_ai_profile": {
      devProfile = String((args as { profile?: unknown }).profile ?? "balanced");
      return devModels() as T;
    }
    case "start_semantic_backfill":
    case "stop_semantic_backfill":
      return devModels() as T;
    case "list_conversations":
      return devConversations() as T;
    case "load_conversation": {
      const id = String(args?.["id"] ?? "");
      const found = DEV_THREADS.get(id);
      if (!found) throw { code: "CFG_INVALID", message: "No such conversation." };
      const detail: ConversationDetail = {
        id,
        title: found.title,
        scope: found.scope,
        turns: found.turns,
      };
      return detail as T;
    }
    case "save_turn": {
      const into = args?.["conversation"];
      const turn = args?.["turn"] as NewTurn;
      const id = typeof into === "string" ? into : `01DEV${DEV_THREADS.size}`;
      const existing = DEV_THREADS.get(id);
      const stored: StoredTurn = {
        question: turn.question,
        answer: turn.answer,
        thorough: turn.thorough,
        model: turn.model,
        scope: turn.scope,
        citations: turn.citations,
        excluded: turn.excluded,
        // Derived in Rust from the stored citations; the fixture keeps its own
        // hands off it rather than inventing a second rule.
        projects: [],
        usage: turn.usage,
        askedMs: Date.now(),
      };
      const title = existing?.title ?? turn.question.slice(0, 60);
      DEV_THREADS.set(id, {
        title,
        scope: turn.scope,
        updatedMs: Date.now(),
        turns: [...(existing?.turns ?? []), stored],
      });
      const saved: SavedTurn = { id, title };
      return saved as T;
    }
    case "rename_conversation": {
      const id = String(args?.["id"] ?? "");
      const row = DEV_THREADS.get(id);
      if (row) DEV_THREADS.set(id, { ...row, title: String(args?.["title"] ?? "") });
      return undefined as T;
    }
    case "delete_conversation": {
      // Soft in the app; the fixture has no `status` column to flip, and a
      // browser-only stub that pretended otherwise would be modelling a
      // database it does not have.
      DEV_THREADS.delete(String(args?.["id"] ?? ""));
      return undefined as T;
    }
    case "cancel_ask":
      return true as T;
    case "forget_conversation":
      return undefined as T;
    case "release_model":
      return devModels() as T;
    case "open_path":
    case "reveal_path":
      // Nothing to open in a browser; the command exists and succeeds.
      return undefined as T;
    case "reindex":
      // The count of folders asked, which is what the notice reads back. The
      // sweep is Rust and there is none in a browser, so the numbers on the
      // page stay put — the button's own feedback is the thing under test.
      return WORKSPACES.length as T;
    default:
      throw { code: "UI_UNEXPECTED", message: `No fixture for "${cmd}".` };
  }
}

/**
 * A scripted answer, so the streaming path, the Markdown renderer, the
 * diagram and the sandboxed preview are all exercised in `pnpm dev` without a
 * model. The delay is real: a stream that arrives instantly hides every layout
 * problem that only appears while text is growing.
 */
export async function mockAsk(
  question: string,
  thorough: boolean,
  onEvent: (e: AskEvent) => void,
  priorTurns = 0,
): Promise<string> {
  // The handle first, exactly as the command does: Stop has to have something
  // to cancel for the whole time it is on screen.
  onEvent({ kind: "started", id: "dev-ask" });
  // The stages a real run emits, with delays long enough to actually see —
  // a fixture that skips straight to tokens hides every layout problem the
  // waiting state has.
  onEvent({ kind: "stage", stage: "retrieving", detail: "Searching your files" });
  await sleep(400);
  if (priorTurns === 0) {
    onEvent({
      kind: "stage",
      stage: "loading",
      detail: "Loading Qwen 3.5 4B — first question of the session",
    });
    await sleep(900);
  }

  const sources: Citation[] = [
    {
      id: "E1",
      path: "/Users/you/melp/services/vault/README.md",
      relativePath: "services/vault/README.md",
      location: "services/vault/README.md:14",
      line: 14,
      excerpt:
        "Enclave stores documents encrypted at rest; the vault service holds the keys and never the plaintext.",
      provenance: "exact",
    },
    {
      id: "E2",
      path: "/Users/you/melp/services/vault/src/auth/token.rs",
      relativePath: "services/vault/src/auth/token.rs",
      location: "services/vault/src/auth/token.rs:88",
      line: 88,
      excerpt:
        "Tokens are rotated every fifteen minutes; a refresh that arrives after the window is rejected rather than extended.",
      provenance: "exact",
    },
  ];

  onEvent({
    kind: "sources",
    hits: sources,
    excluded: [
      {
        relativePath: "notes/summary-generated.md",
        reason: "written by Marrow itself, so it cannot support a claim",
      },
    ],
    bytes: 4820,
    distinctSources: 2,
    boundary: "local",
    model: "Qwen 3.5 4B",
  });

  onEvent({
    kind: "stage",
    stage: "thinking",
    detail: thorough ? "Reading the evidence and reasoning" : "Reading the evidence",
  });
  await sleep(500);

  if (thorough) {
    for (const t of [
      "The question is about how the vault handles keys. ",
      "E1 says the plaintext never reaches the vault service. ",
      "E2 is about token rotation, which is adjacent but not the same thing. ",
      "I should answer from E1 and mention E2 only as context.",
    ]) {
      await sleep(90);
      onEvent({ kind: "thinking", text: t });
    }
  }

  const answer = `The vault holds **keys**, never plaintext [E1].

Documents are encrypted at rest before they reach it, so a compromise of the
vault service yields key material and nothing readable on its own.

| Component | Holds | Rotates |
| --- | --- | --- |
| vault | keys | every 15 min [E2] |
| store | ciphertext | never |

\`\`\`mermaid
flowchart LR
  A[Document] -->|encrypt| B[Ciphertext]
  B --> C[(Store)]
  A -.key.-> D[Vault]
  D -->|rotate 15m| D
\`\`\`

A refresh arriving after the window is rejected rather than extended [E2].

\`\`\`html
<div style="font:14px system-ui;padding:12px">
  <b>Key rotation</b><br>
  <span id="t">15:00</span> until the next rotation
  <script>
    let s = 900;
    setInterval(() => {
      s = s > 0 ? s - 1 : 900;
      document.getElementById('t').textContent =
        String(Math.floor(s / 60)).padStart(2, '0') + ':' + String(s % 60).padStart(2, '0');
    }, 1000);
  </script>
</div>
\`\`\`
`;

  for (const word of answer.split(/(?<=\s)/)) {
    await sleep(12);
    onEvent({ kind: "token", text: word });
  }

  onEvent({
    kind: "done",
    promptTokens: 604 + priorTurns * 90,
    outputTokens: 168,
    thinkingTokens: thorough ? 406 : 0,
    // A follow-up reuses the whole preamble; the first turn has nothing to
    // reuse. Modelled here so the footer is exercised both ways.
    cachedPrefixTokens: priorTurns === 0 ? 0 : 487 + (priorTurns - 1) * 90,
    stopReason: "stop",
    elapsedMs: thorough ? 5200 : 2100,
  });
  void question;
  return "dev-ask";
}

function sleep(ms: number) {
  return new Promise<void>((r) => setTimeout(r, ms));
}
