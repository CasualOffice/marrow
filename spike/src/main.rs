//! M0 spike — throwaway. Measures the three things the roadmap needs numbers for:
//!   1. `ignore` walk rate over a real tree
//!   2. blake3 hash throughput
//!   3. SQLite batched insert rate through a single writer
//!
//! Prints aggregates only. Never prints a filename.

use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        eprintln!("usage: marrow-spike <root>...");
        std::process::exit(2);
    }

    // ---- 1. walk ---------------------------------------------------------
    let t = Instant::now();
    let mut builder = ignore::WalkBuilder::new(&roots[0]);
    for r in &roots[1..] {
        builder.add(r);
    }
    builder
        .hidden(false) // see dotfiles; policy decides later, not the walker
        .follow_links(false) // WS-005
        .git_ignore(true) // FS-002
        .git_global(false)
        .parents(false);
    // NOTE: filter_entry REPLACES the predicate, it does not chain. One closure,
    // one set — calling it in a loop silently keeps only the last pattern.
    const NOISE: &[&str] = &[
        "node_modules", ".git", "target", "build", "dist", ".venv", "venv",
        "__pycache__", ".gradle", ".next", "vendor", "Pods", "DerivedData",
    ];
    builder.filter_entry(|e| {
        let n = e.file_name().to_string_lossy();
        !NOISE.iter().any(|d| *d == n)
    });

    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    let mut dirs = 0u64;
    let mut errors = 0u64;
    let mut symlinks = 0u64;
    for res in builder.build() {
        match res {
            Ok(e) => match e.file_type() {
                Some(ft) if ft.is_file() => {
                    let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push((e.into_path(), len));
                }
                Some(ft) if ft.is_dir() => dirs += 1,
                Some(ft) if ft.is_symlink() => symlinks += 1,
                _ => {}
            },
            Err(_) => errors += 1,
        }
    }
    let walk = t.elapsed();
    let total_bytes: u64 = files.iter().map(|(_, n)| *n).sum();

    println!("== walk ==");
    println!("files            {}", files.len());
    println!("dirs             {dirs}");
    println!("symlinks         {symlinks}");
    println!("errors           {errors}");
    println!("bytes            {:.2} GB", total_bytes as f64 / 1.073_741_824e9);
    println!("elapsed          {:.2} s", walk.as_secs_f64());
    println!("rate             {:.0} files/s", files.len() as f64 / walk.as_secs_f64());

    // ---- 1b. composition -------------------------------------------------
    use std::collections::HashMap;
    let mut ext: HashMap<String, (u64, u64)> = HashMap::new(); // ext -> (count, bytes)
    for (p, len) in &files {
        let e = p
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "(none)".into());
        let slot = ext.entry(e).or_insert((0, 0));
        slot.0 += 1;
        slot.1 += len;
    }
    let mut by_count: Vec<_> = ext.iter().collect();
    by_count.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    println!("\n== extensions (top 25 by count) ==");
    println!("{:<12} {:>7} {:>10}", "ext", "files", "MB");
    for (e, (c, b)) in by_count.iter().take(25) {
        println!("{:<12} {:>7} {:>10.1}", e, c, *b as f64 / 1.048_576e6);
    }
    println!("distinct exts    {}", ext.len());

    let mut buckets = [0u64; 5];
    for (_, len) in &files {
        let i = match *len {
            0..=65_535 => 0,
            65_536..=1_048_575 => 1,
            1_048_576..=52_428_799 => 2,
            52_428_800..=524_287_999 => 3,
            _ => 4,
        };
        buckets[i] += 1;
    }
    println!("\n== size buckets ==");
    for (label, n) in ["<64KB", "<1MB", "<50MB", "<500MB", ">=500MB"].iter().zip(buckets) {
        println!("{:<10} {:>7}  ({:.1}%)", label, n, 100.0 * n as f64 / files.len() as f64);
    }

    // ---- 2. hash ---------------------------------------------------------
    // Cap at 50 MB/file so one huge video doesn't dominate the throughput number.
    const CAP: u64 = 50 * 1024 * 1024;
    let t = Instant::now();
    let mut hashed = 0u64;
    let mut hashed_bytes = 0u64;
    let mut unreadable = 0u64;
    let mut buf = vec![0u8; 262_144];
    let mut hashes: Vec<[u8; 32]> = Vec::with_capacity(files.len());
    for (p, len) in &files {
        if *len > CAP {
            continue;
        }
        match std::fs::File::open(p) {
            Ok(mut f) => {
                let mut h = blake3::Hasher::new();
                loop {
                    match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            h.update(&buf[..n]);
                            hashed_bytes += n as u64;
                        }
                        Err(_) => {
                            unreadable += 1;
                            break;
                        }
                    }
                }
                hashes.push(*h.finalize().as_bytes());
                hashed += 1;
            }
            Err(_) => unreadable += 1,
        }
    }
    let hash = t.elapsed();
    println!("\n== blake3 ==");
    println!("hashed           {hashed}");
    println!("unreadable       {unreadable}");
    println!("bytes            {:.2} GB", hashed_bytes as f64 / 1.073_741_824e9);
    println!("elapsed          {:.2} s", hash.as_secs_f64());
    println!("throughput       {:.0} MB/s", hashed_bytes as f64 / 1.048_576e6 / hash.as_secs_f64());
    println!("rate             {:.0} files/s", hashed as f64 / hash.as_secs_f64());

    // dedup: how much of the corpus is duplicate content? (FS-008)
    let mut sorted = hashes.clone();
    sorted.sort_unstable();
    let unique = {
        let mut u = sorted.clone();
        u.dedup();
        u.len()
    };
    println!("unique hashes    {unique} of {} ({:.1}% dupes)",
        hashes.len(),
        100.0 * (hashes.len().saturating_sub(unique)) as f64 / hashes.len().max(1) as f64);

    // ---- 3. sqlite -------------------------------------------------------
    let db = std::env::temp_dir().join("marrow-spike.sqlite");
    let _ = std::fs::remove_file(&db);
    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;
         CREATE TABLE files(
           file_id TEXT PRIMARY KEY,
           path TEXT NOT NULL,
           size_bytes INTEGER NOT NULL,
           content_hash TEXT NOT NULL,
           observed_at INTEGER NOT NULL);
         CREATE INDEX idx_path ON files(path);
         CREATE INDEX idx_hash ON files(content_hash);",
    )
    .expect("schema");

    let now = 1_756_512_000_000i64;
    let t = Instant::now();
    let mut rows = 0u64;
    {
        let tx = conn.unchecked_transaction().expect("tx");
        {
            let mut st = tx
                .prepare("INSERT INTO files VALUES (?1,?2,?3,?4,?5)")
                .expect("prep");
            for (i, (p, len)) in files.iter().enumerate() {
                let h = hashes.get(i).map(hex).unwrap_or_default();
                st.execute(rusqlite::params![
                    ulid::Ulid::new().to_string(),
                    p.to_string_lossy(),
                    *len as i64,
                    h,
                    now
                ])
                .expect("insert");
                rows += 1;
            }
        }
        tx.commit().expect("commit");
    }
    let ins = t.elapsed();
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);

    println!("\n== sqlite ==");
    println!("rows             {rows}");
    println!("elapsed          {:.2} s", ins.as_secs_f64());
    println!("rate             {:.0} rows/s", rows as f64 / ins.as_secs_f64());
    println!("db size          {:.1} MB", db_bytes as f64 / 1.048_576e6);
    println!("bytes/row        {:.0}", db_bytes as f64 / rows.max(1) as f64);

    // query latency on a warm index
    let t = Instant::now();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM files WHERE path LIKE '%.md'", [], |r| r.get(0))
        .unwrap_or(0);
    println!("LIKE scan        {:.1} ms ({n} hits)", t.elapsed().as_secs_f64() * 1000.0);

    let _ = std::fs::remove_file(&db);
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
