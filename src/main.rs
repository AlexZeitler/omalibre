//! omalibre - a terminal ebook reader with library management.

mod annotation;
mod app;
mod doc;
mod epub;
mod export;
mod find;
mod identity;
mod image;
mod journal;
mod layout;
mod library;
mod mcp;
mod paths;
mod search;
mod shelf;
mod theme;
mod ui;

use anyhow::{Context, Result};
use app::App;
use clap::Parser;
use epub::Book;
use identity::BookId;
use journal::Journal;
use layout::LayoutOptions;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "omalibre", version, about = "Terminal ebook reader")]
struct Cli {
    /// EPUB file to open. Left out, the library opens.
    file: Option<PathBuf>,

    /// Print the book's structure and exit, without starting the reader.
    #[arg(long)]
    inspect: bool,

    /// List the images of every chapter and whether they can be read.
    #[arg(long)]
    images: bool,

    /// Backend to use for --images: kitty, sixel or half-blocks.
    #[arg(long, value_name = "BACKEND")]
    image_backend: Option<String>,

    /// Print the parsed blocks of one chapter, by its number in the spine.
    #[arg(long, value_name = "N")]
    dump: Option<usize>,

    /// Read a directory into the library and exit.
    #[arg(long, value_name = "DIR")]
    scan: Option<PathBuf>,

    /// List the library and exit.
    #[arg(long)]
    list: bool,

    /// Print the books read most recently as JSON and exit. Default five.
    #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "5")]
    recent: Option<usize>,

    /// With --list: print JSON rather than a table.
    #[arg(long)]
    json: bool,

    /// With --list: only books whose title, author, series or tags match.
    #[arg(long, value_name = "TEXT")]
    filter: Option<String>,

    /// Write the library as Markdown for a search engine to index.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "")]
    export: Option<String>,

    /// Export every book, even those unchanged since the last run.
    #[arg(long)]
    force: bool,

    /// After exporting, have qmd re-index. Add --embed for vectors as well.
    #[arg(long)]
    reindex: bool,

    /// With --reindex: also update the embeddings, which takes a while.
    #[arg(long)]
    embed: bool,

    /// Open a book at a place: an exported Markdown file, a qmd hit
    /// (qmd://collection/path.md:line), or a book id.
    #[arg(long, value_name = "REF")]
    open: Option<String>,

    /// Start an MCP server on stdio, so a model can read the library.
    #[arg(long)]
    mcp: bool,

    /// Search the whole library and open a hit.
    #[arg(long, value_name = "TEXT")]
    find: Option<String>,

    /// With --open: the chapter to open, when the reference names none.
    #[arg(long, value_name = "HREF")]
    chapter: Option<String>,

    /// With --open: jump to the first occurrence of this text.
    #[arg(long, value_name = "TEXT")]
    at: Option<String>,
}

fn main() -> Result<()> {
    restore_sigpipe();
    let cli = Cli::parse();

    if let Some(dir) = &cli.scan {
        return scan_directory(dir);
    }
    if cli.list {
        return list_library(cli.json, cli.filter.as_deref().unwrap_or(""));
    }
    if let Some(count) = cli.recent {
        return list_recent(count);
    }
    if let Some(dir) = &cli.export {
        return export_library(dir, cli.force, cli.reindex, cli.embed);
    }
    if let Some(query) = &cli.find {
        return find_and_open(query);
    }
    if cli.mcp {
        let config = paths::Config::load()?;
        return mcp::serve(config.journal_dir()?);
    }
    if let Some(reference) = &cli.open {
        return open_reference(reference, cli.chapter.as_deref(), cli.at.as_deref());
    }

    let Some(file) = cli.file.clone() else {
        return browse();
    };
    let book = Book::open(&file).with_context(|| format!("cannot read {}", file.display()))?;

    if let Some(chapter) = cli.dump {
        return dump_chapter(book, chapter.saturating_sub(1));
    }
    if cli.images {
        let backend = cli
            .image_backend
            .as_deref()
            .and_then(image::Backend::parse)
            .unwrap_or(image::Backend::HalfBlocks);
        return list_images(book, backend);
    }
    if cli.inspect {
        return inspect(book);
    }
    run(book, file)
}

/// Restores the usual reaction to a pipe nobody reads any more.
///
/// Rust ignores SIGPIPE, so a write into such a pipe returns an error and
/// `println!` turns that into a panic: `omalibre --list | head` ended in a
/// backtrace instead of simply stopping. The default action ends the process
/// quietly, which is how every other command behaves in a pipeline.
fn restore_sigpipe() {
    // Sound here and nowhere later: no other thread runs yet, so nothing can
    // observe the disposition while it changes.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

/// Prepares what a fresh installation needs before the first screen appears.
///
/// The reader is made for Omarchy, so following the theme is not an extra step
/// somebody has to find in a README: the template goes in on its own and the
/// first session already has the right colours. Both parts leave an existing
/// installation untouched.
fn first_start() -> Result<()> {
    paths::Config::write_default_if_missing()?;
    if theme::install_template() {
        println!("Hooked omalibre into your Omarchy theme.");
    }
    Ok(())
}

/// Opens the library and reads whichever book is picked, until the reader quits.
///
/// Shelf and reader are separate screens with one terminal between them. The
/// journal is replayed on every return, so a reading position or annotation made
/// just now shows on the shelf straight away.
fn browse() -> Result<()> {
    first_start()?;
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;

    let backend = choose_image_backend(config.images.as_deref())?;
    let mut terminal = ratatui::init();
    let cell = cell_size(&terminal);
    let mut theme = theme::Watcher::new();

    let result = (|| -> Result<()> {
        loop {
            let state = Journal::replay(&journal_dir)?;
            let mut shelf = shelf::Shelf::new(&state);
            if shelf.total() == 0 {
                ratatui::restore();
                println!("The library is empty. Read one in with:");
                println!("  omalibre --scan ~/path/to/books");
                return Ok(());
            }

            // The shelf runs until a book is picked or the reader quits.
            let picked = loop {
                theme.refresh();
                let colours = theme.theme();
                terminal.draw(|frame| ui::draw_shelf(frame, &mut shelf, &colours))?;
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match shelf.handle_key(key) {
                            shelf::Action::Open { id, path } => break Some((id, path)),
                            shelf::Action::Quit => break None,
                            shelf::Action::None => {}
                        }
                    }
                    _ => {}
                }
            };

            let Some((id, path)) = picked else {
                return Ok(());
            };
            let book = match Book::open(&path) {
                Ok(book) => book,
                Err(_) => continue,
            };

            let mut journal = Journal::open(&journal_dir)?;
            journal.assume_written(state.position(&id).cloned());
            let options = LayoutOptions {
                max_width: config.max_width.unwrap_or(u16::MAX),
            };
            let mut app = App::new(book, id, journal, &state, options)?;
            app.set_image_backend(backend, cell);

            repaint_everything(&mut terminal)?;
            event_loop(&mut terminal, &mut app)?;
            app.save_position();

            // Back to the shelf, with a clean screen: the reader may have left
            // pixels behind that no cell redraw would remove.
            repaint_everything(&mut terminal)?;
            if app.should_quit_program {
                return Ok(());
            }
        }
    })();

    ratatui::restore();
    result
}

/// Searches the library and opens whichever hit is picked.
///
/// The hits are gathered before the terminal is taken over, so progress from a
/// direct search is visible and a slow run can be interrupted.
fn find_and_open(query: &str) -> Result<()> {
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;

    let mut last = std::time::Instant::now();
    let results = find::find(query, &state, 40, &mut |title| {
        // Only every so often: a line per book would scroll the report away.
        if last.elapsed() > std::time::Duration::from_millis(400) {
            last = std::time::Instant::now();
            println!("  searching {title} ...");
        }
    })?;

    if results.hits.is_empty() {
        println!("nothing found for {query:?}");
        if matches!(results.source, find::Source::Direct) {
            println!("\nFor faster and broader search, index the library:");
            println!("  omalibre --export --reindex");
        }
        return Ok(());
    }

    // Pick a hit, then open the book there.
    let backend = choose_image_backend(config.images.as_deref())?;
    let mut terminal = ratatui::init();
    let cell = cell_size(&terminal);
    let mut theme = theme::Watcher::new();
    let picked = pick_hit(&mut terminal, &results, &mut theme);
    let outcome = (|| -> Result<()> {
        let Some(index) = picked? else { return Ok(()) };
        let hit = &results.hits[index];
        let path = find::file_of(hit, &state)?;
        let book = Book::open(&path).with_context(|| format!("cannot read {}", path.display()))?;

        let id = hit.book.clone();
        let mut journal = Journal::open(&journal_dir)?;
        journal.assume_written(state.position(&id).cloned());
        let options = LayoutOptions {
            max_width: config.max_width.unwrap_or(u16::MAX),
        };
        let mut app = App::new(book, id, journal, &state, options)?;
        app.set_image_backend(backend, cell);
        if let Some(href) = &hit.chapter_href {
            app.go_to_href(href);
        }
        // The passage, so the reader lands on the sentence rather than the chapter.
        app.search_for(hit.passage.clone().unwrap_or_else(|| query.to_string()));

        repaint_everything(&mut terminal)?;
        event_loop(&mut terminal, &mut app)?;
        app.save_position();
        Ok(())
    })();

    ratatui::restore();
    outcome
}

/// Shows the hits and returns the chosen one.
fn pick_hit(
    terminal: &mut ratatui::DefaultTerminal,
    results: &find::Results,
    theme: &mut theme::Watcher,
) -> Result<Option<usize>> {
    let mut cursor = 0usize;
    let mut scroll = 0usize;
    let last = results.hits.len().saturating_sub(1);

    loop {
        theme.refresh();
        let colours = theme.theme();
        // Three rows per hit, so the visible count follows the window height.
        let per_screen = ((terminal.size()?.height.saturating_sub(1)) / 3).max(1) as usize;
        if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + per_screen {
            scroll = cursor + 1 - per_screen;
        }
        terminal.draw(|frame| ui::draw_hits(frame, results, cursor, scroll, &colours))?;

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                ratatui::crossterm::event::KeyCode::Char('q')
                | ratatui::crossterm::event::KeyCode::Esc => return Ok(None),
                ratatui::crossterm::event::KeyCode::Char('j')
                | ratatui::crossterm::event::KeyCode::Down => cursor = (cursor + 1).min(last),
                ratatui::crossterm::event::KeyCode::Char('k')
                | ratatui::crossterm::event::KeyCode::Up => cursor = cursor.saturating_sub(1),
                ratatui::crossterm::event::KeyCode::Char('G') => cursor = last,
                ratatui::crossterm::event::KeyCode::Char('g') => cursor = 0,
                ratatui::crossterm::event::KeyCode::Enter
                | ratatui::crossterm::event::KeyCode::Char('l') => return Ok(Some(cursor)),
                _ => {}
            },
            _ => {}
        }
    }
}

/// Writes the library out as Markdown.
fn export_library(dir: &str, force: bool, reindex: bool, embed: bool) -> Result<()> {
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    let dir = if dir.is_empty() {
        export::default_dir()?
    } else {
        PathBuf::from(shellexpand(dir))
    };

    println!("exporting to {} ...", dir.display());
    let report = export::export(&dir, &state, force, &journal_dir)?;
    println!(
        "\n{} books, {} chapters, {} annotations written; {} unchanged",
        report.books, report.chapters, report.annotations, report.unchanged
    );
    if !report.skipped.is_empty() {
        println!("\n{} could not be read:", report.skipped.len());
        for (slug, err) in report.skipped.iter().take(10) {
            println!("  {slug}: {err}");
        }
    }
    if reindex {
        return run_qmd(&dir, embed);
    }
    println!("\nTo index it:");
    println!("  qmd collection add {} --name books", dir.display());
    println!("  qmd embed");
    println!("\nOr let omalibre do it: --export --reindex [--embed]");
    Ok(())
}

/// Hands the export to qmd for indexing.
///
/// `qmd update` re-indexes the collections it already knows. A first run has none,
/// so a failure is not an error here: it means the collection has to be created,
/// and that is a decision about naming which belongs to the user.
fn run_qmd(dir: &Path, embed: bool) -> Result<()> {
    println!("\nrunning qmd update ...");
    let status = std::process::Command::new("qmd").arg("update").status();
    match status {
        Ok(status) if status.success() => {}
        Ok(_) => {
            println!("\nqmd update did not succeed. If no collection points here yet:");
            println!("  qmd collection add {} --name books", dir.display());
            return Ok(());
        }
        Err(err) => {
            println!("\ncannot run qmd: {err}");
            println!("  qmd collection add {} --name books", dir.display());
            return Ok(());
        }
    }

    if embed {
        println!("\nrunning qmd embed ...");
        // Embedding runs a local model over everything new; it is slow by nature,
        // which is why it takes a flag of its own.
        let _ = std::process::Command::new("qmd").arg("embed").status();
    } else {
        println!("\nFor semantic search, update the vectors as well:");
        println!("  qmd embed");
    }
    Ok(())
}

/// Opens a book at a place named by an exported file or a book id.
///
/// This is the way back from a search hit: the hit names a file, the file names
/// the book and chapter, and `--at` puts the reader on the passage rather than at
/// the top of a chapter.
fn open_reference(reference: &str, chapter: Option<&str>, at: Option<&str>) -> Result<()> {
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;

    // An exported file carries its origin; anything else is taken as a book id.
    let mut from_line = None;
    let (book_id, from_file) = if reference.starts_with("sha256:") {
        (reference.to_string(), None)
    } else {
        let (path, line) = resolve_hit(reference)?;
        let origin = export::origin_of(&path)?;
        // A search hit names a line. Its text is the passage that matched, so it
        // makes a far better landing point than the top of a chapter.
        if let Some(line) = line {
            from_line = line_text(&path, line);
        }
        (origin.book, origin.chapter)
    };

    let id = BookId::from(book_id);
    let record = state
        .book(&id)
        .with_context(|| format!("no book with id {} in the library", id))?;
    let path = record
        .path()
        .cloned()
        .with_context(|| format!("no file recorded for {}", record.display_title()))?;

    let book = Book::open(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let target = chapter.map(str::to_string).or(from_file);

    run_at(book, path, target, at.map(str::to_string).or(from_line))
}

/// Turns a reference into a file and, where one is given, a line number.
///
/// Accepts a plain path, a path with `:line`, and a qmd hit of the form
/// `qmd://collection/relative/path.md:line`. A qmd hit names the collection
/// rather than the directory behind it, so the export directory stands in for it:
/// that is the collection omalibre wrote.
fn resolve_hit(reference: &str) -> Result<(PathBuf, Option<usize>)> {
    let (body, line) = split_line_suffix(reference);

    if let Some(rest) = body.strip_prefix("qmd://") {
        // Drop the collection name; what follows is relative to the export.
        let relative = rest.split_once('/').map(|(_, rest)| rest).unwrap_or(rest);
        let path = export::default_dir()?.join(relative);
        anyhow::ensure!(
            path.exists(),
            "{} does not exist. If the collection points elsewhere, pass the file path instead.",
            path.display()
        );
        return Ok((path, line));
    }
    Ok((PathBuf::from(shellexpand(body)), line))
}

/// Splits a trailing `:123` off a reference, leaving a Windows-style drive letter
/// or a `sha256:` prefix alone.
fn split_line_suffix(reference: &str) -> (&str, Option<usize>) {
    match reference.rsplit_once(':') {
        Some((body, tail)) => match tail.parse::<usize>() {
            Ok(line) if !body.is_empty() => (body, Some(line)),
            _ => (reference, None),
        },
        None => (reference, None),
    }
}

/// The text of one line of a file, trimmed to something worth searching for.
///
/// A whole line can be a long paragraph; the first words are enough to find the
/// passage and are less likely to differ from the book by a stray character.
fn line_text(path: &Path, line: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = text.lines().nth(line.saturating_sub(1))?.trim();
    if raw.is_empty() || raw.starts_with("---") {
        return None;
    }
    // Markdown decoration would not appear in the book's own text.
    let cleaned = raw.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
    let words: Vec<&str> = cleaned.split_whitespace().take(8).collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

/// Expands a leading `~`, which a shell would otherwise have done.
fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest).to_string_lossy().into_owned(),
            None => path.to_string(),
        },
        None => path.to_string(),
    }
}

/// Reads a directory into the library.
fn scan_directory(dir: &PathBuf) -> Result<()> {
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;
    let state = Journal::replay(&journal_dir)?;
    let mut journal = Journal::open(&journal_dir)?;

    println!("scanning {} ...", dir.display());
    let mut count = 0usize;
    let report = library::scan(dir, &mut journal, &state, &mut |path| {
        count += 1;
        // A scan over hundreds of files should say that it is working.
        if count % 25 == 0 {
            println!("  {count} files ... {}", path.display());
        }
    })?;

    println!(
        "\n{} files seen, {} added, {} moved",
        report.seen, report.added, report.moved
    );
    if !report.unreadable.is_empty() {
        println!("\n{} could not be read:", report.unreadable.len());
        for (path, err) in &report.unreadable {
            println!("  {}: {err}", path.display());
        }
    }
    Ok(())
}

/// Prints the library.
fn list_library(as_json: bool, needle: &str) -> Result<()> {
    let config = paths::Config::load()?;
    let state = Journal::replay(&config.journal_dir()?)?;
    let mut entries = library::entries(&state);
    library::sort(&mut entries, library::Order::Title);
    let entries = library::filter(&entries, needle);

    if as_json {
        return print_books(&entries);
    }
    if entries.is_empty() {
        if needle.trim().is_empty() {
            println!("The library is empty. Read one in with:");
            println!("  omalibre --scan ~/path/to/books");
        } else {
            println!("No book matches {needle}.");
        }
        return Ok(());
    }

    if needle.trim().is_empty() {
        println!("{} books\n", entries.len());
    } else {
        println!("{} books matching {needle}\n", entries.len());
    }
    for entry in &entries {
        let record = &entry.record;
        let series = match (&record.series, record.series_index) {
            (Some(name), Some(index)) => format!("  [{name} {index}]"),
            (Some(name), None) => format!("  [{name}]"),
            _ => String::new(),
        };
        let flag = if record.missing { " (missing)" } else { "" };
        println!(
            "  {:<34} {:<26}{series}{flag}",
            truncate(&record.display_title(), 34),
            truncate(&record.display_authors(), 26)
        );
    }
    Ok(())
}

/// Prints the books read most recently, as JSON.
///
/// `--list` draws a table for a person. This prints the same library for a
/// program: the Omarchy bar widget draws the rows and hands an id straight back
/// to `--open` when someone picks one. A book that was never opened carries no
/// timestamp and is left out, because recent means read, not owned.
fn list_recent(count: usize) -> Result<()> {
    let config = paths::Config::load()?;
    let state = Journal::replay(&config.journal_dir()?)?;
    let mut entries = library::entries(&state);
    entries.retain(|entry| entry.last_read.is_some());
    library::sort(&mut entries, library::Order::Recent);
    entries.truncate(count);

    print_books(&entries)
}

/// Writes books as JSON, one object each, for a program to read.
fn print_books(entries: &[library::Entry]) -> Result<()> {
    let books: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id.to_string(),
                "title": entry.record.display_title(),
                "authors": entry.record.authors,
                // Seconds, not nanoseconds: the bar widget parses this with
                // JavaScript's Date, which reads at most milliseconds.
                "lastRead": entry
                    .last_read
                    .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                "missing": entry.record.missing,
            })
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&books)?);
    Ok(())
}

fn run(book: Book, path: PathBuf) -> Result<()> {
    run_at(book, path, None, None)
}

/// Opens a book, optionally at a chapter and a passage.
fn run_at(book: Book, path: PathBuf, chapter: Option<String>, at: Option<String>) -> Result<()> {
    first_start()?;
    let config = paths::Config::load()?;
    let journal_dir = config.journal_dir()?;

    let id = BookId::of_file(&path)?;
    let state = Journal::replay(&journal_dir)?;
    let restore = state.position(&id).cloned();
    let mut journal = Journal::open(&journal_dir)?;
    journal.assume_written(restore.clone());

    // Record the book so a journal read elsewhere can name it without opening
    // the file. Only when something changed, otherwise every start would add a
    // line that says nothing new.
    let here = path.canonicalize().unwrap_or_else(|_| path.clone());
    let known = state.book(&id);
    let unchanged = known.is_some_and(|record| {
        record.paths.contains(&here)
            && !record.missing
            && (record.title.is_some() || book.metadata.title.is_none())
    });
    if !unchanged {
        journal.append(
            &id,
            journal::Payload::BookSeen {
                title: book.metadata.title.clone(),
                authors: book.metadata.authors.clone(),
                path: here,
            },
        )?;
    }

    let options = LayoutOptions {
        max_width: config.max_width.unwrap_or(u16::MAX),
    };
    let mut app = App::new(book, id, journal, &state, options)?;
    if let Some(chapter) = chapter {
        app.go_to_href(&chapter);
    }
    if let Some(text) = at {
        app.search_for(text);
    }

    // Ask the terminal before the alternate screen is up, so its answer cannot
    // be mistaken for user input later on.
    let backend = choose_image_backend(config.images.as_deref())?;
    let mut terminal = ratatui::init();
    app.set_image_backend(backend, cell_size(&terminal));
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();

    // Saving after restoring the terminal keeps an error message visible.
    app.save_position();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_token = None;
    // Whether the previous frame put pixels on screen. Asking the app instead
    // would come too late: on a chapter change the old chapter's pictures are
    // already gone and the new one's are not rendered yet.
    let mut pixels_on_screen = false;

    while !app.should_quit {
        // Pixel pictures are not part of the cell buffer, so a diffed redraw
        // leaves them wherever no character happens to overwrite them. Once the
        // view has moved, the screen has to be wiped and painted afresh.
        let token = app.frame_token();
        let moved = last_token.is_some_and(|last| last != token);
        // Either direction matters: pictures that are on screen have to go, and
        // pictures that are about to appear need a clean surface.
        let wiping = moved && (pixels_on_screen || app.has_pixel_images());
        last_token = Some(token);

        {
            // The wipe above and the painting below are one frame to the reader,
            // so the terminal is told to hold the display until both are done.
            let _frame = HeldDisplay::begin();
            if wiping {
                repaint_everything(terminal)?;
            }
            // A theme switch replaces the colours under us. Checking after each
            // key is enough and costs one stat call.
            if app.refresh_theme() {
                repaint_everything(terminal)?;
            }

            let mut placements = Vec::new();
            terminal.draw(|frame| placements = ui::draw(frame, app))?;
            place_images(app, &placements)?;
            pixels_on_screen = !placements.is_empty();
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                app.handle_key(key);
                // A held-down key delivers keys faster than a chapter full of
                // pictures can be painted. Drawing each one would wipe the
                // screen that often, so what is already waiting is taken now
                // and shown as one frame.
                while !app.should_quit
                    && app.pending_edit.is_none()
                    && event::poll(std::time::Duration::ZERO)?
                {
                    match event::read()? {
                        Event::Key(next) if next.kind == KeyEventKind::Press => {
                            app.handle_key(next)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // A comment is written in the user's editor, which needs the terminal to
        // itself. Only this loop may hand it over and take it back.
        if let Some(request) = app.pending_edit.take() {
            let outcome = with_suspended_terminal(terminal, || edit_note(&request));
            match outcome {
                Ok(text) => app.finish_edit(&request.annotation_id, text),
                Err(err) => app.report(format!("comment not saved: {err:#}")),
            }
        }
    }
    Ok(())
}

/// Keeps the terminal from showing a half-built frame.
///
/// A Sixel picture is part of the screen contents, so moving the view means
/// wiping the screen and painting it again. Without this the empty screen
/// between the two is visible, and scrolling past pictures flickers.
///
/// The mode is DEC 2026, which foot, Ghostty, kitty and others implement. A
/// terminal that does not know it ignores it, as it must for any private mode
/// it does not implement, so there is nothing to detect first.
struct HeldDisplay;

impl HeldDisplay {
    fn begin() -> Self {
        let mut out = std::io::stdout();
        // Failing to hold the display costs a flicker, not correctness, so a
        // write error here is not worth failing the frame over.
        let _ = out.write_all(b"\x1b[?2026h");
        let _ = out.flush();
        HeldDisplay
    }
}

impl Drop for HeldDisplay {
    fn drop(&mut self) {
        // Runs however the frame ended, so an error cannot leave the display
        // frozen.
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();
    }
}

/// Wipes the screen and marks every cell as changed, so the next draw paints
/// everything. Avoids `Terminal::clear`, which asks the terminal for its cursor
/// position and waits for an answer.
fn repaint_everything(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    use ratatui::backend::Backend;
    let size = terminal.size().context("cannot read the terminal size")?;
    terminal.backend_mut().clear().context("cannot clear")?;
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
        .context("cannot reset the buffers")?;
    Ok(())
}

/// Decides how pictures are drawn: a setting wins, otherwise the terminal is
/// asked.
///
/// Asking needs raw mode, because the answer arrives as an escape sequence on
/// stdin. Raw mode is switched off again straight away, so a terminal that stays
/// silent leaves nothing behind.
fn choose_image_backend(setting: Option<&str>) -> Result<image::Backend> {
    if let Some(setting) = setting {
        if let Some(backend) = image::Backend::parse(setting) {
            return Ok(backend);
        }
        // A misspelling must not be read as "no pictures".
        eprintln!("omalibre: unknown images setting {setting:?}, asking the terminal instead");
    }

    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    enable_raw_mode().context("cannot enter raw mode to query the terminal")?;
    let backend = image::detect::detect();
    disable_raw_mode().ok();
    Ok(backend)
}

/// Pixel size of one cell, as reported by the terminal. Falls back to a common
/// default when the terminal does not say.
fn cell_size(terminal: &ratatui::DefaultTerminal) -> image::CellSize {
    use ratatui::crossterm::terminal::window_size;
    let _ = terminal;
    match window_size() {
        Ok(size) if size.width > 0 && size.height > 0 && size.columns > 0 && size.rows > 0 => {
            image::CellSize {
                width: size.width / size.columns,
                height: size.height / size.rows,
            }
        }
        _ => image::CellSize::default(),
    }
}

/// Writes the pictures of the current frame.
///
/// Pixel protocols paint outside the text buffer, so ratatui knows nothing about
/// them. Old pictures are removed first, where the protocol allows addressing
/// them, and each remaining one is placed at the cursor position its reserved
/// lines start at.
fn place_images(app: &App, placements: &[ui::Placement]) -> Result<()> {
    let backend = app.image_backend();
    let clear = image::clear_all(backend);
    if clear.is_none() && placements.is_empty() {
        return Ok(());
    }

    use ratatui::crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
    use ratatui::crossterm::queue;
    let mut out = std::io::stdout();

    if let Some(clear) = clear {
        queue!(out, ratatui::crossterm::style::Print(clear))?;
    }
    for placement in placements {
        queue!(
            out,
            SavePosition,
            MoveTo(placement.column, placement.row),
            ratatui::crossterm::style::Print(&placement.escape),
            RestorePosition
        )?;
    }
    out.flush()?;
    Ok(())
}

/// Hands the terminal to another program and takes it back afterwards.
///
/// Only the modes are switched, and the same terminal object is kept. Two calls
/// are deliberately avoided here. `ratatui::init` would reinstall the panic hook,
/// and `Terminal::clear` asks the terminal for its cursor position, which is a
/// round trip that fails under tmux and over slow links. Resizing the buffers to
/// the current size has the same effect: everything counts as changed and is
/// painted afresh.
fn with_suspended_terminal<T>(
    terminal: &mut ratatui::DefaultTerminal,
    body: impl FnOnce() -> Result<T>,
) -> Result<T> {
    use ratatui::backend::Backend;
    use ratatui::crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::crossterm::{cursor::Show, execute};

    disable_raw_mode().context("cannot leave raw mode")?;
    execute!(std::io::stdout(), LeaveAlternateScreen, Show)
        .context("cannot leave the alternate screen")?;

    let outcome = body();

    enable_raw_mode().context("cannot re-enter raw mode")?;
    execute!(std::io::stdout(), EnterAlternateScreen)
        .context("cannot re-enter the alternate screen")?;
    let size = terminal.size().context("cannot read the terminal size")?;
    terminal.backend_mut().clear().context("cannot clear")?;
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
        .context("cannot reset the buffers")?;
    outcome
}

/// Deletes the scratch file whichever way its function returns.
///
/// An editor that fails to start makes `edit_note` return early. Without this
/// the note and the quoted passage stay on disk after that.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// Opens `$EDITOR` on a scratch file and returns what was written.
///
/// The quoted passage is put below a marker line as a reminder of what the
/// comment refers to. The marker and everything under it is dropped again.
fn edit_note(request: &app::EditRequest) -> Result<String> {
    const MARKER: &str = "--- the highlighted passage, for reference ---";

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    // Not the shared temporary directory. This file holds the note and the
    // passage it belongs to, and there every account on the machine may read
    // it, for as long as the editor stays open.
    let path = paths::scratch_dir()?.join(format!("note-{}.md", std::process::id()));
    let scaffold = format!(
        "{}\n\n{MARKER}\n{}\n",
        request.initial_text,
        request.quote.replace('\n', " ")
    );

    // A run killed outright leaves a file behind, and process ids come round
    // again, so clear the name before claiming it. create_new then refuses
    // anything that appears in between.
    std::fs::remove_file(&path).ok();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    let scratch = Scratch(path.clone());
    // The existing comment goes first so the cursor lands on it.
    file.write_all(scaffold.as_bytes())?;
    drop(file);

    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("cannot run {editor}"))?;

    let text = if status.success() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };
    drop(scratch);

    // Keep only what stands above the marker.
    let body = text.split(MARKER).next().unwrap_or("");
    Ok(body.trim().to_string())
}

/// Prints one chapter's blocks with their kind, to see how markup came through.
fn dump_chapter(mut book: Book, index: usize) -> Result<()> {
    let chapter = book.chapter(index)?;
    println!(
        "{} - {} blocks, {} links, {} anchors\n",
        chapter.href,
        chapter.blocks.len(),
        chapter.links.len(),
        chapter.anchors.len()
    );
    for link in &chapter.links {
        println!(
            "  link  block {:>4} chars {}..{}  -> {}",
            link.block, link.start, link.end, link.target
        );
    }
    if !chapter.links.is_empty() {
        println!();
    }
    for (n, block) in chapter.blocks.iter().enumerate() {
        let kind = match &block.kind {
            doc::BlockKind::Heading(level) => format!("h{level}"),
            doc::BlockKind::Paragraph => "p".to_string(),
            doc::BlockKind::Quote => "quote".to_string(),
            doc::BlockKind::Code => "code".to_string(),
            doc::BlockKind::ListItem { depth, ordinal } => {
                format!("li d{depth} {ordinal:?}")
            }
            doc::BlockKind::Rule => "rule".to_string(),
            doc::BlockKind::Image { src } => format!("img {}", src.as_deref().unwrap_or("-")),
        };
        let text = block.plain_text();
        println!(
            "{n:>4} {kind:<12} {:>4}ch  {}",
            text.chars().count(),
            truncate(&text, 90)
        );
    }
    Ok(())
}

/// Prints every image, whether its bytes can be read and how large it renders.
/// Useful to tell a wrong path from a format the decoder refuses.
fn list_images(mut book: Book, backend: image::Backend) -> Result<()> {
    let mut total = 0;
    let mut readable = 0;
    let mut rendered = 0;
    let mut bytes_out = 0usize;
    println!("backend: {}\n", backend.name());

    for index in 0..book.spine.len() {
        let Ok(chapter) = book.chapter(index) else {
            continue;
        };
        let sources: Vec<(usize, Option<String>)> = chapter
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(block, b)| match &b.kind {
                doc::BlockKind::Image { src } => Some((block, src.clone())),
                _ => None,
            })
            .collect();

        for (block, src) in sources {
            total += 1;
            let Some(src) = src else {
                println!("  ch{index:>3} block{block:>4}  NO SRC");
                continue;
            };
            match book.read_binary(&src) {
                Ok(bytes) => {
                    readable += 1;
                    match image::render(&bytes, 80, 20, backend, 1, image::CellSize::default()) {
                        Ok(picture) if picture.height() > 0 => {
                            rendered += 1;
                            let payload = picture.escape().map(str::len).unwrap_or(0);
                            bytes_out += payload;
                            println!(
                                "  ch{index:>3} block{block:>4}  {:>8} B  {:>3}x{:<3} cells  {:>9} B out  {src}",
                                bytes.len(),
                                picture.width(),
                                picture.height(),
                                payload
                            );
                        }
                        Ok(_) => println!("  ch{index:>3} block{block:>4}  EMPTY        {src}"),
                        Err(err) => {
                            println!("  ch{index:>3} block{block:>4}  UNDECODABLE  {src}: {err}")
                        }
                    }
                }
                Err(err) => println!("  ch{index:>3} block{block:>4}  MISSING      {src}: {err}"),
            }
        }
    }
    println!(
        "\n{total} images, {readable} readable, {rendered} rendered, {bytes_out} B of escapes"
    );
    Ok(())
}

/// Prints metadata and the reading order. Useful to check a book without the
/// full interface.
fn inspect(mut book: Book) -> Result<()> {
    println!("title:      {}", book.title());
    println!("authors:    {}", book.metadata.authors.join(", "));
    println!(
        "language:   {}",
        book.metadata.language.as_deref().unwrap_or("-")
    );
    println!(
        "identifier: {}",
        book.metadata.identifier.as_deref().unwrap_or("-")
    );
    println!("spine:      {} items\n", book.spine.len());

    let count = book.spine.len();
    let mut failures = 0;
    let mut blocks_total = 0;
    for index in 0..count {
        let item = book.spine[index].clone();
        match book.chapter(index) {
            Ok(chapter) => {
                blocks_total += chapter.blocks.len();
                println!(
                    "  {:>3}. {:<48} {:>4} blocks  {}",
                    index + 1,
                    truncate(item.title.as_deref().unwrap_or(&item.href), 48),
                    chapter.blocks.len(),
                    item.href
                );
            }
            Err(err) => {
                failures += 1;
                println!("  {:>3}. FAILED {}: {err:#}", index + 1, item.href);
            }
        }
    }
    println!("\n{blocks_total} blocks total, {failures} chapters failed");
    Ok(())
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes an executable stub that stands in for `$EDITOR`.
    fn stub_editor(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("omalibre-stub-{name}"));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn a_request() -> app::EditRequest {
        app::EditRequest {
            annotation_id: "a1".to_string(),
            initial_text: "what I thought".to_string(),
            quote: "the passage".to_string(),
        }
    }

    /// One test rather than several, because they would fight over `$EDITOR`.
    #[test]
    fn the_scratch_file_stays_private_and_never_outlives_the_edit() {
        let report = std::env::temp_dir().join("omalibre-stub-report");
        std::fs::remove_file(&report).ok();
        let editor = stub_editor(
            "stat",
            &format!(
                "stat -c '%a %n' \"$1\" > {}; printf 'the note\\n' > \"$1\"",
                report.display()
            ),
        );
        // SAFETY: no other test in this crate reads either variable.
        unsafe {
            std::env::remove_var("VISUAL");
            std::env::set_var("EDITOR", &editor);
        }

        let text = edit_note(&a_request()).unwrap();
        assert_eq!(text, "the note");

        let recorded = std::fs::read_to_string(&report).unwrap();
        let (mode, file) = recorded.trim().split_once(' ').unwrap();
        assert_eq!(mode, "600", "the editor saw mode {mode}");

        let dir = paths::scratch_dir().unwrap();
        assert!(
            Path::new(file).starts_with(&dir),
            "{file} is outside {}",
            dir.display()
        );
        assert!(!Path::new(file).exists(), "{file} outlived the edit");

        // An editor that cannot start must not leave the note behind either.
        unsafe { std::env::set_var("EDITOR", dir.join("no-such-editor")) };
        assert!(edit_note(&a_request()).is_err());
        assert!(!Path::new(file).exists(), "{file} survived a failed editor");

        std::fs::remove_file(&editor).ok();
        std::fs::remove_file(&report).ok();
    }
}
