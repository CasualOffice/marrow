//! Conversations: the one thing in this database that is not derived.
//!
//! Everything else the store holds can be rebuilt by reading the user's files
//! again — versions, chunks, parse results, the lot. A conversation cannot: it
//! is what was asked and what came back, and if it is lost there is nowhere to
//! read it from. That is the whole reason it is here rather than in the
//! window's component state, which is where it lived and where quitting the app
//! ended it.
//!
//! Two shapes, and the split matters:
//!
//! - [`ConversationRow`] is the **list**: a title, when it was last touched,
//!   how many turns. Cheap enough to fetch every time the sidebar renders.
//! - [`TurnRow`] is the **thread**: fetched only when a conversation is opened,
//!   because it carries every answer in full.
//!
//! The JSON columns (`citations`, `excluded`, `usage`) are opaque here on
//! purpose. Their shape belongs to whoever renders a conversation, and this
//! crate having an opinion about it would put the window's wire format in the
//! canonical schema. SQLite validates that they are JSON; the caller validates
//! that they are *its* JSON. Same arrangement as `jobs.payload`.

use std::str::FromStr;

use marrow_core::{Code, Error, Result, Timestamp};
use rusqlite::{params, Connection, OptionalExtension};

/// How the answer was produced (GEN-012). Stored as `TEXT` with a CHECK, so a
/// row is readable in a debugger without a lookup table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnMode {
    Fast,
    Thorough,
}

impl TurnMode {
    pub fn as_sql(self) -> &'static str {
        match self {
            TurnMode::Fast => "FAST",
            TurnMode::Thorough => "THOROUGH",
        }
    }

    /// Anything unrecognised reads as `Fast`, which is the conservative
    /// direction: it claims no reasoning happened rather than inventing some.
    fn from_sql(s: &str) -> Self {
        match s {
            "THOROUGH" => TurnMode::Thorough,
            _ => TurnMode::Fast,
        }
    }
}

/// One conversation, as the list shows it.
#[derive(Clone, Debug)]
pub struct ConversationRow {
    pub conversation_id: String,
    pub title: String,
    pub scope: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub turns: i64,
}

/// One exchange, with everything needed to render it exactly as it was.
#[derive(Clone, Debug)]
pub struct TurnRow {
    pub turn_id: String,
    pub ordinal: i64,
    pub question: String,
    pub answer: String,
    pub mode: TurnMode,
    pub model: Option<String>,
    pub scope: Option<String>,
    /// The citation list as JSON, exactly as it was shown with the answer.
    pub citations: String,
    /// What was retrieved and not sent, as JSON.
    pub excluded: String,
    /// Token counts and timings as JSON, or `None` for a turn that never
    /// reached the end of a generation.
    pub usage: Option<String>,
    pub asked_at: Timestamp,
}

/// A turn on its way in. The conversation and the ordinal are the store's to
/// assign — a caller that picked its own ordinal would be racing the writer for
/// the right to number a thread.
#[derive(Clone, Debug)]
pub struct NewTurn {
    pub question: String,
    pub answer: String,
    pub mode: TurnMode,
    pub model: Option<String>,
    pub scope: Option<String>,
    pub citations: String,
    pub excluded: String,
    pub usage: Option<String>,
    pub at: Timestamp,
}

/// The most conversations the sidebar will ever ask for at once.
///
/// Not pagination — a bound. Ten thousand threads is not a list anyone scrolls,
/// and the query that returned them would be paid for on every render of the
/// window's most permanent surface.
pub const MAX_LIST: usize = 200;

/// A conversation's name, derived from the question that started it.
///
/// Cut on a word boundary rather than mid-token, because the list is scanned
/// rather than read and half a word at the end of every row is noise the eye
/// has to step over. An empty question — which the window does not send, but
/// which the schema must survive — becomes a placeholder rather than an empty
/// row that looks like a rendering failure.
pub fn title_from(question: &str) -> String {
    const MAX: usize = 60;
    let flat = question.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "Untitled conversation".to_string();
    }
    if flat.chars().count() <= MAX {
        return flat;
    }
    let cut: String = flat.chars().take(MAX).collect();
    let trimmed = match cut.rsplit_once(' ') {
        // Only when the boundary is late enough to be worth the loss; a
        // question whose first word is 58 characters keeps its 60.
        Some((head, _)) if head.chars().count() >= MAX / 2 => head,
        _ => cut.as_str(),
    };
    format!("{}…", trimmed.trim_end())
}

/// Reject an identifier that is not a ULID before it reaches a query.
///
/// Not injection — every value below is bound. This is so a malformed id fails
/// with a message that says what is wrong, instead of succeeding as a query
/// that matches nothing and reads as "your conversation is gone".
fn check_id(id: &str, what: &str) -> Result<()> {
    ulid::Ulid::from_str(id).map(|_| ()).map_err(|_| {
        Error::new(
            Code::CfgInvalid,
            "That conversation identifier is not one this build can have written. \
             Reopen the conversation from the list.",
        )
        .with_context(format!("{what} = {id:?}"))
    })
}

fn missing(id: &str) -> Error {
    Error::new(
        Code::CfgInvalid,
        "That conversation is no longer here. It may have been deleted from \
         another window; the list has what is left.",
    )
    .with_context(format!("conversation_id = {id:?}"))
}

/// Append a turn, creating the conversation if this is the first one.
///
/// **Creation is lazy on purpose.** A "New conversation" button that writes a
/// row when it is pressed fills the list with empty threads nobody asked a
/// question in — the state a user leaves behind every time they open the app,
/// think better of it, and quit. A conversation exists once something was said
/// in it.
///
/// Returns the conversation the turn landed in, so the caller can go on
/// appending to it.
pub fn append_turn(
    conn: &Connection,
    conversation_id: Option<&str>,
    turn: &NewTurn,
) -> Result<String> {
    let at = turn.at.as_millis();
    let id = match conversation_id {
        Some(existing) => {
            check_id(existing, "conversation_id")?;
            // The status filter is what stops a turn landing in a conversation
            // the user deleted while it was still generating.
            let updated = conn
                .execute(
                    "UPDATE conversations
                        SET updated_at = ?2, scope = ?3
                      WHERE conversation_id = ?1 AND status = 'ACTIVE'",
                    params![existing, at, turn.scope],
                )
                .map_err(|e| crate::map_sqlite(e, "Could not record the conversation's turn."))?;
            if updated == 0 {
                return Err(missing(existing));
            }
            existing.to_string()
        }
        None => {
            let fresh = ulid::Ulid::new().to_string();
            conn.execute(
                "INSERT INTO conversations
                     (conversation_id, title, scope, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![fresh, title_from(&turn.question), turn.scope, at],
            )
            .map_err(|e| crate::map_sqlite(e, "Could not start a conversation in the index."))?;
            fresh
        }
    };

    // Computed inside the same writer op as the insert, so two turns cannot
    // read the same maximum and then both claim it. The UNIQUE constraint is
    // the backstop; this is what stops it firing.
    let ordinal: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM conversation_turns
              WHERE conversation_id = ?1",
            params![id],
            |r| r.get(0),
        )
        .map_err(|e| crate::map_sqlite(e, "Could not find the conversation's next turn."))?;

    conn.execute(
        "INSERT INTO conversation_turns
             (turn_id, conversation_id, ordinal, question, answer, mode, model,
              scope, citations, excluded, usage, asked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            ulid::Ulid::new().to_string(),
            id,
            ordinal,
            turn.question,
            turn.answer,
            turn.mode.as_sql(),
            turn.model,
            turn.scope,
            turn.citations,
            turn.excluded,
            turn.usage,
            at,
        ],
    )
    .map_err(|e| crate::map_sqlite(e, "Could not record the answer in the conversation."))?;

    Ok(id)
}

/// Conversations that have not been deleted, most recently used first.
pub fn list_conversations(conn: &Connection, limit: usize) -> Result<Vec<ConversationRow>> {
    let limit = limit.clamp(1, MAX_LIST) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT c.conversation_id, c.title, c.scope, c.created_at, c.updated_at,
                    (SELECT count(*) FROM conversation_turns t
                      WHERE t.conversation_id = c.conversation_id)
               FROM conversations c
              WHERE c.status = 'ACTIVE'
           ORDER BY c.updated_at DESC
              LIMIT ?1",
        )
        .map_err(|e| crate::map_sqlite(e, "Could not read your conversations."))?;
    let rows = stmt
        .query_map(params![limit], row_to_conversation)
        .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| crate::map_sqlite(e, "Could not read your conversations."))?;
    Ok(rows)
}

/// One conversation and every turn in it, in the order they were asked.
pub fn load_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<(ConversationRow, Vec<TurnRow>)> {
    check_id(conversation_id, "conversation_id")?;
    let head = conn
        .query_row(
            "SELECT c.conversation_id, c.title, c.scope, c.created_at, c.updated_at,
                    (SELECT count(*) FROM conversation_turns t
                      WHERE t.conversation_id = c.conversation_id)
               FROM conversations c
              WHERE c.conversation_id = ?1 AND c.status = 'ACTIVE'",
            params![conversation_id],
            row_to_conversation,
        )
        .optional()
        .map_err(|e| crate::map_sqlite(e, "Could not open that conversation."))?
        .ok_or_else(|| missing(conversation_id))?;

    let mut stmt = conn
        .prepare(
            "SELECT turn_id, ordinal, question, answer, mode, model, scope,
                    citations, excluded, usage, asked_at
               FROM conversation_turns
              WHERE conversation_id = ?1
           ORDER BY ordinal",
        )
        .map_err(|e| crate::map_sqlite(e, "Could not read that conversation's answers."))?;
    let turns = stmt
        .query_map(params![conversation_id], |r| {
            Ok(TurnRow {
                turn_id: r.get(0)?,
                ordinal: r.get(1)?,
                question: r.get(2)?,
                answer: r.get(3)?,
                mode: TurnMode::from_sql(&r.get::<_, String>(4)?),
                model: r.get(5)?,
                scope: r.get(6)?,
                citations: r.get(7)?,
                excluded: r.get(8)?,
                usage: r.get(9)?,
                asked_at: Timestamp::from_millis(r.get(10)?),
            })
        })
        .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| crate::map_sqlite(e, "Could not read that conversation's answers."))?;

    Ok((head, turns))
}

/// Give a conversation a name of the user's own.
///
/// An empty or whitespace-only title is refused rather than stored: a row with
/// no label cannot be picked out of a list, and silently restoring the derived
/// title would look like the rename failed at random.
pub fn rename_conversation(
    conn: &Connection,
    conversation_id: &str,
    title: &str,
    at: Timestamp,
) -> Result<()> {
    check_id(conversation_id, "conversation_id")?;
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(Error::new(
            Code::CfgInvalid,
            "A conversation needs a name to be findable in the list. Type one, or \
             leave the name it already has.",
        ));
    }
    let changed = conn
        .execute(
            "UPDATE conversations SET title = ?2, updated_at = ?3
              WHERE conversation_id = ?1 AND status = 'ACTIVE'",
            params![conversation_id, trimmed, at.as_millis()],
        )
        .map_err(|e| crate::map_sqlite(e, "Could not rename that conversation."))?;
    if changed == 0 {
        return Err(missing(conversation_id));
    }
    Ok(())
}

/// Take a conversation off the list. **Soft delete, always.**
///
/// The rows stay exactly where they are and `status` moves to `DELETED`.
/// Physical deletion happens through the forget path and nowhere else — a
/// mis-click here would otherwise be the one loss in this database that cannot
/// be re-derived from anything.
///
/// `updated_at` is deliberately **not** touched: it records when the
/// conversation was last used, and a restore that came back at the top of the
/// list because it was deleted yesterday would be sorting by the wrong event.
pub fn delete_conversation(conn: &Connection, conversation_id: &str) -> Result<()> {
    check_id(conversation_id, "conversation_id")?;
    let changed = conn
        .execute(
            "UPDATE conversations SET status = 'DELETED'
              WHERE conversation_id = ?1 AND status = 'ACTIVE'",
            params![conversation_id],
        )
        .map_err(|e| crate::map_sqlite(e, "Could not delete that conversation."))?;
    if changed == 0 {
        return Err(missing(conversation_id));
    }
    Ok(())
}

fn row_to_conversation(r: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationRow> {
    Ok(ConversationRow {
        conversation_id: r.get(0)?,
        title: r.get(1)?,
        scope: r.get(2)?,
        created_at: Timestamp::from_millis(r.get(3)?),
        updated_at: Timestamp::from_millis(r.get(4)?),
        turns: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn store() -> Store {
        Store::open_in_memory().expect("open")
    }

    fn turn(question: &str, answer: &str, at: i64) -> NewTurn {
        NewTurn {
            question: question.into(),
            answer: answer.into(),
            mode: TurnMode::Fast,
            model: Some("qwen3.5-4b".into()),
            scope: None,
            citations: "[]".into(),
            excluded: "[]".into(),
            usage: None,
            at: Timestamp::from_millis(at),
        }
    }

    #[test]
    fn a_conversation_round_trips_with_its_citations() {
        // The whole point of persisting a thread. A conversation you can return
        // to but whose citations are gone is a different conversation: the
        // claims are still there and nothing says where they came from.
        let s = store();
        let citations = r#"[{"id":"E1","path":"/root/lease.md","relativePath":"lease.md",
             "location":"lease.md:14","line":14,"excerpt":"renews in 2031","provenance":"exact"}]"#;
        let mut first = turn(
            "When does the lease renew?",
            "In 2031 [E1].",
            1_700_000_000_000,
        );
        first.citations = citations.into();
        first.excluded =
            r#"[{"relativePath":"notes.md","reason":"written by Marrow itself"}]"#.into();
        first.usage = Some(r#"{"outputTokens":41,"stopReason":"stop"}"#.into());
        first.mode = TurnMode::Thorough;
        first.scope = Some("services/STT".into());

        let id = s
            .append_turn(None, first.clone())
            .expect("first turn creates the conversation");
        s.append_turn(
            Some(id.clone()),
            turn("And the rent?", "2,400 a month [E1].", 1_700_000_001_000),
        )
        .expect("second turn");

        let conn = s.reader().expect("reader");
        let (head, turns) = load_conversation(&conn, &id).expect("load");
        assert_eq!(head.title, "When does the lease renew?");
        assert_eq!(head.turns, 2);
        assert_eq!(head.scope.as_deref(), None, "the second turn was unscoped");

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].ordinal, 1);
        assert_eq!(turns[1].ordinal, 2, "order is the thread's, not the ULID's");
        assert_eq!(turns[0].question, "When does the lease renew?");
        assert_eq!(turns[0].answer, "In 2031 [E1].");
        assert_eq!(turns[0].mode, TurnMode::Thorough);
        assert_eq!(turns[0].model.as_deref(), Some("qwen3.5-4b"));
        assert_eq!(turns[0].scope.as_deref(), Some("services/STT"));
        assert_eq!(
            turns[0].citations, citations,
            "byte-for-byte what was shown"
        );
        assert!(turns[0].excluded.contains("written by Marrow itself"));
        assert!(turns[0].usage.as_deref().unwrap_or("").contains("41"));
        assert_eq!(turns[0].asked_at.as_millis(), 1_700_000_000_000);
    }

    #[test]
    fn a_deleted_conversation_leaves_the_list_without_leaving_the_database() {
        // Soft delete. The one thing here that cannot be re-derived from the
        // user's files must not be removable by a mis-click.
        let s = store();
        let keep = s.append_turn(None, turn("keep me", "ok", 10)).expect("a");
        let drop = s.append_turn(None, turn("drop me", "ok", 20)).expect("b");

        s.delete_conversation(drop.clone()).expect("delete");

        let conn = s.reader().expect("reader");
        let listed = list_conversations(&conn, 50).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].conversation_id, keep);

        let rows: i64 = conn
            .query_row("SELECT count(*) FROM conversations", [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 2, "the row is still there, with status = DELETED");
        let turns: i64 = conn
            .query_row(
                "SELECT count(*) FROM conversation_turns WHERE conversation_id = ?1",
                params![drop],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(turns, 1, "and so is what was said in it");

        // And it cannot be reopened or written to while it is deleted, which is
        // what stops a still-running answer resurrecting it half-way.
        assert!(load_conversation(&conn, &drop).is_err());
        assert!(s.append_turn(Some(drop), turn("q", "a", 30)).is_err());
    }

    #[test]
    fn the_list_is_ordered_by_when_each_thread_was_last_used() {
        // Not by when it was started: a conversation you came back to this
        // morning belongs above one you abandoned last week.
        let s = store();
        let old = s.append_turn(None, turn("first", "a", 100)).expect("a");
        let new = s.append_turn(None, turn("second", "a", 200)).expect("b");
        s.append_turn(Some(old.clone()), turn("again", "a", 300))
            .expect("c");

        let conn = s.reader().expect("reader");
        let listed = list_conversations(&conn, 50).expect("list");
        let ids: Vec<&str> = listed.iter().map(|c| c.conversation_id.as_str()).collect();
        assert_eq!(ids, vec![old.as_str(), new.as_str()]);
    }

    #[test]
    fn renaming_replaces_the_derived_title_and_refuses_an_empty_one() {
        let s = store();
        let id = s
            .append_turn(None, turn("what is STT?", "a", 1))
            .expect("a");
        s.rename_conversation(id.clone(), "STT service".into(), Timestamp::from_millis(2))
            .expect("rename");

        let conn = s.reader().expect("reader");
        assert_eq!(
            load_conversation(&conn, &id).expect("load").0.title,
            "STT service"
        );
        assert!(s
            .rename_conversation(id, "   ".into(), Timestamp::from_millis(3))
            .is_err());
    }

    #[test]
    fn a_title_is_cut_at_a_word_rather_than_mid_token() {
        assert_eq!(title_from("  what   is  STT? "), "what is STT?");
        assert_eq!(title_from(""), "Untitled conversation");
        let long = "when does the lease on the second floor renew and what happens to the rent";
        let t = title_from(long);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 61, "{t}");
        assert!(!t.contains("wha…"), "cut on a space: {t}");
        // A single token longer than the cap has no boundary to cut on, and
        // half of it beats none of it.
        let solid = "a".repeat(90);
        assert_eq!(title_from(&solid).chars().count(), 61);
    }

    #[test]
    fn an_identifier_that_was_never_ours_is_refused_rather_than_missed() {
        // A query bound with a nonsense id matches nothing, which renders as
        // "your conversation is gone" — a much worse thing to be told than
        // "that is not an identifier".
        let s = store();
        let conn = s.reader().expect("reader");
        let e = load_conversation(&conn, "../../etc/passwd").expect_err("refused");
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.message().contains("identifier"), "{}", e.message());
    }
}
