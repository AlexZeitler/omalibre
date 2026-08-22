//! An MCP server over stdio, so a model can read the library.
//!
//! What it offers is what a search index does badly: the library with its
//! metadata, a chapter as text, the annotations you wrote, and where you left
//! off. Library-wide search by meaning stays with a tool built for it; see
//! `export`.
//!
//! The protocol is JSON-RPC 2.0 with one message per line. Three methods carry
//! everything: `initialize`, `tools/list` and `tools/call`. Writing that by hand
//! keeps this program free of an async runtime it has no other use for.
//!
//! Everything is read fresh from the journal on each call, so an annotation made
//! a moment ago in the reader is visible here without a re-export.

use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{Journal, State};
use crate::{export, library, search};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// The protocol version this server speaks.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Chapter text is capped, because a model's context is finite and a chapter of a
/// technical book can run long. The cap is generous enough for a whole chapter in
/// almost every book.
const MAX_CHAPTER_CHARS: usize = 60_000;

pub fn serve(journal_dir: PathBuf) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            // A malformed line is answered rather than ignored, so a client is not
            // left waiting.
            Err(err) => {
                let response = error_response(Value::Null, -32700, &format!("parse error: {err}"));
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
                continue;
            }
        };

        let Some(response) = handle(&request, &journal_dir) else {
            // Notifications carry no id and take no answer.
            continue;
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

/// Answers one request. `None` for a notification.
fn handle(request: &Value, journal_dir: &PathBuf) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let id = request.get("id").cloned();

    // A notification has no id; `initialized` is the common one.
    let Some(id) = id else {
        return None;
    };

    match method {
        "initialize" => Some(result_response(id, initialize(request))),
        "tools/list" => Some(result_response(id, json!({ "tools": tool_definitions() }))),
        "tools/call" => Some(match call_tool(request, journal_dir) {
            Ok(content) => result_response(
                id,
                json!({ "content": [{ "type": "text", "text": content }] }),
            ),
            // A failed tool call is reported inside the result, not as a protocol
            // error: the call was well formed, the answer is that it did not work.
            Err(err) => result_response(
                id,
                json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": format!("{err:#}") }]
                }),
            ),
        }),
        "ping" => Some(result_response(id, json!({}))),
        _ => Some(error_response(
            id,
            -32601,
            &format!("unknown method: {method}"),
        )),
    }
}

fn initialize(request: &Value) -> Value {
    // Echo the client's version when it is one we know, so a newer client is not
    // told to downgrade for no reason.
    let asked = request
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    let version = if asked >= PROTOCOL_VERSION {
        asked
    } else {
        PROTOCOL_VERSION
    };

    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "omalibre",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// The tools, with their schemas.
fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_books",
            "description": "List books in the library. Filter matches title, author, series and tags.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "Substring to match" },
                    "order": {
                        "type": "string",
                        "enum": ["title", "author", "series", "recent"],
                        "description": "Sort order, default title"
                    },
                    "limit": { "type": "integer", "description": "Maximum rows, default 50" }
                }
            }
        }),
        json!({
            "name": "get_book",
            "description": "Metadata, chapter list and reading position of one book.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book": { "type": "string", "description": "Book id or a title to match" }
                },
                "required": ["book"]
            }
        }),
        json!({
            "name": "read_chapter",
            "description": "The text of one chapter as Markdown.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book": { "type": "string", "description": "Book id or a title to match" },
                    "chapter": {
                        "type": "string",
                        "description": "Chapter number in the reading order, or its href"
                    }
                },
                "required": ["book", "chapter"]
            }
        }),
        json!({
            "name": "search_book",
            "description": "Find a phrase in one book. Returns chapter, position and surrounding text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book": { "type": "string", "description": "Book id or a title to match" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "description": "Maximum hits, default 20" }
                },
                "required": ["book", "query"]
            }
        }),
        json!({
            "name": "annotations",
            "description": "Highlights and comments. Without a book, those of the whole library.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "book": { "type": "string", "description": "Book id or a title to match" },
                    "with_notes_only": {
                        "type": "boolean",
                        "description": "Only annotations that carry a comment"
                    }
                }
            }
        }),
        json!({
            "name": "reading_positions",
            "description": "Where reading left off, most recent first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Maximum rows, default 20" }
                }
            }
        }),
    ]
}

fn call_tool(request: &Value, journal_dir: &PathBuf) -> Result<String> {
    let params = request.get("params").context("no params")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("no tool name")?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);

    // Read fresh every time: the reader may have written since the last call.
    let state = Journal::replay(journal_dir)?;

    match name {
        "list_books" => list_books(&state, args),
        "get_book" => get_book(&state, args),
        "read_chapter" => read_chapter(&state, args),
        "search_book" => search_book(&state, args),
        "annotations" => annotations(&state, args),
        "reading_positions" => reading_positions(&state, args),
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

// ----- the tools -----

fn list_books(state: &State, args: &Value) -> Result<String> {
    let filter = args.get("filter").and_then(Value::as_str).unwrap_or("");
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(500) as usize;
    let order = match args.get("order").and_then(Value::as_str) {
        Some("author") => library::Order::Author,
        Some("series") => library::Order::Series,
        Some("recent") => library::Order::Recent,
        _ => library::Order::Title,
    };

    let mut entries = library::entries(state);
    library::sort(&mut entries, order);
    let shown = library::filter(&entries, filter);

    let mut out = format!(
        "{} of {} books, ordered by {}\n\n",
        shown.len().min(limit),
        entries.len(),
        order.label()
    );
    for entry in shown.iter().take(limit) {
        let record = &entry.record;
        out.push_str(&format!("- {}", record.display_title()));
        if !record.authors.is_empty() {
            out.push_str(&format!(" — {}", record.display_authors()));
        }
        if let Some(series) = &record.series {
            match record.series_index {
                Some(at) => out.push_str(&format!(" [{series} {at}]")),
                None => out.push_str(&format!(" [{series}]")),
            }
        }
        if let Some(at) = entry.last_read {
            out.push_str(&format!(" · read {}", at.format("%Y-%m-%d")));
        }
        out.push_str(&format!("\n  id: {}\n", entry.id));
    }
    if shown.len() > limit {
        out.push_str(&format!("\n({} more not shown)\n", shown.len() - limit));
    }
    Ok(out)
}

fn get_book(state: &State, args: &Value) -> Result<String> {
    let (id, record) = resolve_book(state, args)?;
    let mut book = open_book(&record)?;

    let mut out = format!("# {}\n\n", record.display_title());
    out.push_str(&format!("- id: {id}\n"));
    out.push_str(&format!("- author: {}\n", record.display_authors()));
    if let Some(series) = &record.series {
        out.push_str(&format!("- series: {series}\n"));
    }
    if !record.tags.is_empty() {
        out.push_str(&format!("- tags: {}\n", record.tags.join(", ")));
    }
    if let Some(language) = &record.language {
        out.push_str(&format!("- language: {language}\n"));
    }
    for path in &record.paths {
        out.push_str(&format!("- file: {}\n", path.display()));
    }
    if let Some(locator) = state.position(&id) {
        out.push_str(&format!(
            "- reading position: {} block {}\n",
            locator.href, locator.block
        ));
    }
    let marks = state.annotations(&id);
    out.push_str(&format!("- annotations: {}\n", marks.len()));

    out.push_str("\n## Chapters\n\n");
    for (index, item) in book.spine.iter().enumerate() {
        let title = item
            .title
            .clone()
            .unwrap_or_else(|| format!("Chapter {}", index + 1));
        out.push_str(&format!("{:>3}. {title}  ({})\n", index + 1, item.href));
    }
    // Touch the archive so a broken file is reported here rather than later.
    let _ = book.chapter(0);
    Ok(out)
}

fn read_chapter(state: &State, args: &Value) -> Result<String> {
    let (_, record) = resolve_book(state, args)?;
    let mut book = open_book(&record)?;
    let wanted = args
        .get("chapter")
        .and_then(Value::as_str)
        .context("no chapter given")?;
    let index = resolve_chapter(&book, wanted)?;

    let chapter = book.chapter(index)?;
    let title = book
        .spine
        .get(index)
        .and_then(|item| item.title.clone())
        .unwrap_or_else(|| format!("Chapter {}", index + 1));

    let body =
        export::chapter_markdown_titled(&chapter, Some(&record.display_title()), Some(&title));
    let mut out = format!(
        "chapter {} of {} · {} · {}\n\n",
        index + 1,
        book.spine.len(),
        title,
        chapter.href
    );
    if body.chars().count() > MAX_CHAPTER_CHARS {
        let cut: String = body.chars().take(MAX_CHAPTER_CHARS).collect();
        out.push_str(&cut);
        out.push_str("\n\n[truncated]\n");
    } else {
        out.push_str(&body);
    }
    Ok(out)
}

fn search_book(state: &State, args: &Value) -> Result<String> {
    let (_, record) = resolve_book(state, args)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .context("no query given")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(200) as usize;

    let mut book = open_book(&record)?;
    let mut out = String::new();
    let mut found = 0usize;

    for index in 0..book.spine.len() {
        if found >= limit {
            break;
        }
        let Ok(chapter) = book.chapter(index) else {
            continue;
        };
        let hits = search::find_all(&chapter, query);
        if hits.is_empty() {
            continue;
        }
        let title = book
            .spine
            .get(index)
            .and_then(|item| item.title.clone())
            .unwrap_or_else(|| format!("Chapter {}", index + 1));

        for (block, offset) in hits {
            if found >= limit {
                break;
            }
            found += 1;
            let text = chapter
                .blocks
                .get(block)
                .map(|b| b.plain_text())
                .unwrap_or_default();
            out.push_str(&format!(
                "- chapter {} ({}) · {}\n  {}\n",
                index + 1,
                title,
                chapter.href,
                context_around(&text, offset, query.chars().count())
            ));
        }
    }

    if found == 0 {
        return Ok(format!(
            "no match for {query:?} in {}",
            record.display_title()
        ));
    }
    Ok(format!(
        "{found} matches for {query:?} in {}\n\n{out}",
        record.display_title()
    ))
}

fn annotations(state: &State, args: &Value) -> Result<String> {
    let notes_only = args
        .get("with_notes_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // One book, or the whole library.
    let books: Vec<(BookId, crate::journal::BookRecord)> = match args.get("book") {
        Some(Value::String(_)) => {
            let (id, record) = resolve_book(state, args)?;
            vec![(id, record)]
        }
        _ => state
            .books()
            .map(|(id, record)| (id.clone(), record.clone()))
            .collect(),
    };

    let mut out = String::new();
    let mut total = 0usize;
    for (id, record) in books {
        let marks: Vec<_> = state
            .annotations(&id)
            .into_iter()
            .filter(|a| !notes_only || a.has_note())
            .collect();
        if marks.is_empty() {
            continue;
        }
        total += marks.len();
        out.push_str(&format!(
            "\n## {} — {}\n\n",
            record.display_title(),
            record.display_authors()
        ));
        for mark in marks {
            out.push_str(&format!("- {} · {}\n", mark.color.label(), mark.href));
            out.push_str(&format!("  > {}\n", mark.quote.replace('\n', " ")));
            if let Some(note) = &mark.note {
                out.push_str(&format!("  note: {}\n", note.replace('\n', " ")));
            }
        }
    }
    if total == 0 {
        return Ok("no annotations".to_string());
    }
    Ok(format!("{total} annotations\n{out}"))
}

fn reading_positions(state: &State, args: &Value) -> Result<String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(200) as usize;

    let mut entries = library::entries(state);
    entries.retain(|entry| entry.last_read.is_some());
    library::sort(&mut entries, library::Order::Recent);

    if entries.is_empty() {
        return Ok("nothing read yet".to_string());
    }
    let mut out = format!("{} books with a reading position\n\n", entries.len());
    for entry in entries.iter().take(limit) {
        let position = state.position(&entry.id);
        out.push_str(&format!(
            "- {} — {}\n  last read: {}\n",
            entry.record.display_title(),
            entry.record.display_authors(),
            entry
                .last_read
                .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default()
        ));
        if let Some(locator) = position {
            out.push_str(&format!(
                "  at: {} block {}\n  id: {}\n",
                locator.href, locator.block, entry.id
            ));
        }
    }
    Ok(out)
}

// ----- shared helpers -----

/// Finds a book by id or by a title that contains the given text.
///
/// A model will more often have a title than a hash, so both work. An ambiguous
/// title is refused rather than guessed, with the candidates named.
fn resolve_book(state: &State, args: &Value) -> Result<(BookId, crate::journal::BookRecord)> {
    let wanted = args
        .get("book")
        .and_then(Value::as_str)
        .filter(|b| !b.trim().is_empty())
        .context("no book given")?;

    if wanted.starts_with("sha256:") {
        let id = BookId::from(wanted.to_string());
        let record = state
            .book(&id)
            .with_context(|| format!("no book with id {wanted}"))?;
        return Ok((id, record.clone()));
    }

    let needle = wanted.trim().to_lowercase();
    let matches: Vec<(BookId, crate::journal::BookRecord)> = state
        .books()
        .filter(|(_, record)| record.display_title().to_lowercase().contains(&needle))
        .map(|(id, record)| (id.clone(), record.clone()))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("no book matching {wanted:?}"),
        1 => Ok(matches.into_iter().next().expect("checked")),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .take(10)
                .map(|(id, record)| format!("{} ({id})", record.display_title()))
                .collect();
            anyhow::bail!(
                "{} books match {wanted:?}; name one by id:\n{}",
                matches.len(),
                names.join("\n")
            )
        }
    }
}

fn open_book(record: &crate::journal::BookRecord) -> Result<Book> {
    let path = record
        .path()
        .with_context(|| format!("no file recorded for {}", record.display_title()))?;
    Book::open(path).with_context(|| format!("cannot read {}", path.display()))
}

/// Accepts a chapter number or an href.
fn resolve_chapter(book: &Book, wanted: &str) -> Result<usize> {
    if let Ok(number) = wanted.trim().parse::<usize>() {
        let index = number.saturating_sub(1);
        anyhow::ensure!(
            index < book.spine.len(),
            "chapter {number} is past the end; the book has {}",
            book.spine.len()
        );
        return Ok(index);
    }
    // An href, exactly or by file name, as links inside books are not always
    // written the way the container spells them.
    if let Some(index) = book.spine.iter().position(|item| item.href == wanted) {
        return Ok(index);
    }
    let name = std::path::Path::new(wanted).file_name();
    book.spine
        .iter()
        .position(|item| std::path::Path::new(&item.href).file_name() == name)
        .with_context(|| format!("no chapter {wanted:?} in this book"))
}

/// A window of text around a match, for a hit list.
fn context_around(text: &str, offset: usize, length: usize) -> String {
    const BEFORE: usize = 60;
    const AFTER: usize = 120;
    let chars: Vec<char> = text.chars().collect();
    let start = offset.saturating_sub(BEFORE);
    let end = (offset + length + AFTER).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    #[test]
    fn initialize_reports_the_protocol_and_tools() {
        let response = handle(
            &request("initialize", json!({ "protocolVersion": PROTOCOL_VERSION })),
            &PathBuf::from("/nonexistent"),
        )
        .expect("an answer");
        let result = &response["result"];
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "omalibre");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn a_newer_client_version_is_echoed() {
        let response = handle(
            &request("initialize", json!({ "protocolVersion": "2025-06-18" })),
            &PathBuf::from("/nonexistent"),
        )
        .expect("an answer");
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn every_tool_has_a_name_and_a_schema() {
        for tool in tool_definitions() {
            assert!(tool["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn tools_list_answers_with_all_of_them() {
        let response =
            handle(&request("tools/list", json!({})), &PathBuf::from("/x")).expect("an answer");
        let tools = response["result"]["tools"].as_array().expect("array");
        assert_eq!(tools.len(), 6);
    }

    #[test]
    fn a_notification_gets_no_answer() {
        let notification = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle(&notification, &PathBuf::from("/x")).is_none());
    }

    #[test]
    fn an_unknown_method_is_an_error() {
        let response =
            handle(&request("nonsense", json!({})), &PathBuf::from("/x")).expect("an answer");
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn a_failed_call_is_reported_in_the_result() {
        // No journal there, so the call cannot succeed.
        let response = handle(
            &request(
                "tools/call",
                json!({ "name": "get_book", "arguments": { "book": "nothing" } }),
            ),
            &PathBuf::from("/nonexistent-journal"),
        )
        .expect("an answer");
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["content"][0]["text"].is_string());
    }

    #[test]
    fn context_is_cut_around_the_match() {
        let text = "a".repeat(300);
        let window = context_around(&text, 150, 3);
        assert!(window.starts_with('…'));
        assert!(window.ends_with('…'));
        assert!(window.chars().count() < text.chars().count());
    }

    #[test]
    fn context_at_the_start_has_no_leading_ellipsis() {
        let window = context_around("short text here", 0, 5);
        assert!(!window.starts_with('…'));
        assert!(!window.ends_with('…'));
    }
}
