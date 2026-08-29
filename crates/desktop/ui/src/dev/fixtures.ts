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
  FileDetail,
  FileRow,
  IndexHealth,
  Region,
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
 * Three workspaces, each degraded differently, because the sidebar and Status
 * have to make all three legible at once (GUI §11):
 *   melp      healthy, but with cloud-only files it never read
 *   pictures  a third of it recorded from metadata alone
 *   icloud    registered and completely empty
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
  },
  {
    name: "pictures",
    path: "/Users/dev/Pictures",
    files: 3478,
    chunks: 11_895,
    contentBytes: 214_000_000,
    cloudOnly: 412,
    unindexed: 1207,
  },
  {
    name: "icloud",
    path: "/Users/dev/Library/Mobile Documents",
    files: 0,
    chunks: 0,
    contentBytes: 0,
    cloudOnly: 0,
    unindexed: 0,
  },
];

const HEALTH: IndexHealth = {
  files: 12_913,
  chunks: 60_105,
  contentBytes: 1_394_000_000,
  cloudOnly: 412,
  schemaVersion: 7,
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
    case "open_path":
    case "reveal_path":
      // Nothing to open in a browser; the command exists and succeeds.
      return undefined as T;
    default:
      throw { code: "UI_UNEXPECTED", message: `No fixture for "${cmd}".` };
  }
}
