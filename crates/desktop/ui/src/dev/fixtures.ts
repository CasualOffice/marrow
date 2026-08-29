/**
 * Development fixtures — **dev only**.
 *
 * `api.ts` reaches for this file only when `import.meta.env.DEV` is true *and*
 * the Tauri IPC bridge is absent, i.e. when the UI is opened in a plain browser
 * via `pnpm dev`. The branch is statically false in a production build, so
 * Rollup drops both the branch and this module: nothing here ships.
 *
 * The data mirrors design/*.dc.html so the running app can be compared with the
 * mockups side by side.
 */

import type {
  FileDetail,
  IndexHealth,
  SearchHit,
  SearchResponse,
  WorkspaceRow,
} from "../api";

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
    relativePath: "services/vault/migrations/0014_session.sql",
    line: 3,
    breadcrumb: "migrations › 0014_session",
    excerpt: "CREATE TABLE refresh_tokens (\n  jti TEXT PRIMARY KEY,",
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
        location: `${s.relativePath}${suffix}:${s.line}`,
        fileId: `${s.fileId}${suffix}`,
      });
    }
  }
  return many;
}

const WORKSPACES: WorkspaceRow[] = [
  { name: "melp", path: "/Users/dev/melp", files: 9435 },
  { name: "pictures", path: "/Users/dev/Pictures", files: 3478 },
  { name: "icloud", path: "/Users/dev/Library/Mobile Documents", files: 0 },
];

const HEALTH: IndexHealth = {
  files: 12971,
  chunks: 60105,
  contentBytes: 1_400_000_000,
  cloudOnly: 412,
  schemaVersion: 7,
};

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
        matched: 42,
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
    case "file_detail": {
      const path = String(args?.["path"] ?? "");
      const d: FileDetail = {
        path,
        fileId: "01M17KJXJ2K51K4824XQ56H2Q7",
        workspace: "melp",
        sizeBytes: 14_100_000,
        contentHash: "blake3:9f2a1c8e4d",
        mime: "text/x-rust",
        modifiedMs: now - 2 * DAY,
        versions: 3,
        chunks: 18,
        tierState: "resident",
        citable: !path.includes("/agent/"),
        previousPaths: [`${ROOT}/services/vault/src/token.rs`],
        embeddedMetadata: null,
        structure: null,
      };
      return d as T;
    }
    case "read_region": {
      const around = args?.["aroundLine"];
      if (typeof around !== "number") return SOURCE as T;
      const pad: string[] = [];
      for (let n = Math.max(1, around - 40); n < 138; n += 1) pad.push("");
      return [...pad, ...SOURCE] as T;
    }
    default:
      throw { code: "UI_UNEXPECTED", message: `No fixture for "${cmd}".` };
  }
}
