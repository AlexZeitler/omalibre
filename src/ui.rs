//! Drawing the reader.

use crate::annotation::{Annotation, Color as MarkColor, text_color_on};
use crate::app::{App, Mode};
use crate::layout::{Line as SourceLine, LineKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

/// Columns kept free left and right of the text.
const SIDE_MARGIN: u16 = 2;

fn rgb((r, g, b): crate::theme::Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// Where a picture goes on screen, for the backends that paint past the text
/// buffer. Collected during the draw and written afterwards.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Column and row on screen, zero-based.
    pub column: u16,
    pub row: u16,
    pub escape: String,
}

pub fn draw(frame: &mut Frame, app: &mut App) -> Vec<Placement> {
    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
    let (text_area, status_area) = (areas[0], areas[1]);

    // One column between the margins carries the annotation marks, so a
    // highlight is visible even where the colour is hard to see.
    let gutter = Rect {
        x: text_area.x + SIDE_MARGIN.saturating_sub(1),
        y: text_area.y,
        width: 1,
        height: text_area.height,
    };
    let inner = Rect {
        x: text_area.x + SIDE_MARGIN + 1,
        y: text_area.y,
        width: text_area.width.saturating_sub(SIDE_MARGIN * 2 + 1),
        height: text_area.height,
    };

    app.prepare(inner.width, inner.height);
    draw_text(frame, inner, app);
    draw_gutter(frame, gutter, app);
    draw_status(frame, status_area, app);

    match app.mode {
        Mode::Contents => {
            draw_contents(frame, text_area, app);
            // An overlay covers the text, so pictures must not be painted over it.
            return Vec::new();
        }
        Mode::Annotations => {
            draw_annotations(frame, text_area, app);
            return Vec::new();
        }
        Mode::Help => {
            draw_help(frame, text_area, app);
            return Vec::new();
        }
        _ => {}
    }
    placements(inner, app)
}

/// Collects the pictures visible in this frame, with the screen position each
/// one starts at. Only their first line matters: the protocol paints downwards
/// from there.
fn placements(area: Rect, app: &App) -> Vec<Placement> {
    let mut out = Vec::new();
    for (row, line) in app.visible_lines().iter().enumerate() {
        let LineKind::Image { row: pixel_row, .. } = line.kind else {
            continue;
        };
        // Only the top row of a picture places it.
        if pixel_row != 0 {
            continue;
        }
        let Some(rendered) = app.image_at(line.block) else {
            continue;
        };
        let Some(escape) = rendered.escape() else {
            continue;
        };
        if escape.is_empty() {
            continue;
        }
        // The terminal paints these pixels itself, so nothing clips them at the
        // edge of the text: a picture whose lower half is below the fold would
        // cover the status line. It waits until the whole of it fits, which is
        // one more line of scrolling. Half block pictures are ordinary cells and
        // never get here.
        if row + rendered.height() > area.height as usize {
            continue;
        }
        out.push(Placement {
            column: area.x + line.indent,
            row: area.y + row as u16,
            escape: escape.to_string(),
        });
    }
    out
}

/// What a single character should look like, before spans are merged.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    /// Colour of the annotation covering this character, if one does.
    mark: Option<MarkColor>,
    selected: bool,
    at_cursor: bool,
    has_note: bool,
    /// Part of a search match, and whether it is the one the reader is on.
    match_here: bool,
    current_match: bool,
}

fn draw_text(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let scroll = app.scroll();
    let marks = app.annotations_here();
    let selection = app.selection();
    let cursor = app.cursor_position();
    let search = app.search();

    let lines: Vec<Line> = app
        .visible_lines()
        .iter()
        .enumerate()
        .map(|(row, line)| match line.kind {
            // A picture line is pixels, not text. Pixel protocols paint past the
            // text buffer, so their lines are left blank here and filled in after
            // the draw.
            LineKind::Image { row: pixel_row, .. } => match app.image_at(line.block) {
                Some(rendered) => match rendered.cells() {
                    Some(_) => image_row(rendered, pixel_row, line.indent),
                    None => Line::from(""),
                },
                None => render_line(
                    line,
                    scroll + row,
                    &marks,
                    selection,
                    cursor,
                    area.width,
                    &theme,
                    search,
                ),
            },
            _ => render_line(
                line,
                scroll + row,
                &marks,
                selection,
                cursor,
                area.width,
                &theme,
                search,
            ),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Paints one row of a picture. Each cell shows the upper half block, with the
/// foreground carrying the upper pixel and the background the lower one.
fn image_row(rendered: &crate::image::Rendered, row: u16, indent: u16) -> Line<'static> {
    let Some(cells) = rendered.cells().and_then(|rows| rows.get(row as usize)) else {
        return Line::from("");
    };
    let mut spans = Vec::with_capacity(cells.len() + 1);
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent as usize)));
    }
    // Merge neighbouring cells that share both colours, which keeps the escape
    // sequences short on the flat areas most illustrations have.
    let mut current: Option<(crate::image::Cell, usize)> = None;
    for cell in cells {
        match &mut current {
            Some((last, count)) if last == cell => *count += 1,
            Some((last, count)) => {
                spans.push(half_block(*last, *count));
                current = Some((*cell, 1));
            }
            None => current = Some((*cell, 1)),
        }
    }
    if let Some((cell, count)) = current {
        spans.push(half_block(cell, count));
    }
    Line::from(spans)
}

fn half_block(cell: crate::image::Cell, count: usize) -> Span<'static> {
    let (ur, ug, ub) = cell.upper;
    let (lr, lg, lb) = cell.lower;
    Span::styled(
        "▀".repeat(count),
        Style::default()
            .fg(Color::Rgb(ur, ug, ub))
            .bg(Color::Rgb(lr, lg, lb)),
    )
}

fn render_line(
    source: &SourceLine,
    line_index: usize,
    marks: &[&Annotation],
    selection: Option<((usize, usize), (usize, usize))>,
    cursor: Option<(usize, usize)>,
    width: u16,
    theme: &crate::theme::Theme,
    search: &crate::search::Search,
) -> Line<'static> {
    let mut spans = Vec::new();
    if source.indent > 0 {
        spans.push(Span::raw(" ".repeat(source.indent as usize)));
    }

    // A listing's backdrop covers the indent as well, so the block has a clean
    // left edge.
    if source.kind == LineKind::Code {
        spans.clear();
        if source.indent > 0 {
            spans.push(Span::styled(
                " ".repeat(source.indent as usize),
                Style::default().bg(rgb(theme.code_background)),
            ));
        }
    }

    match source.kind {
        LineKind::Rule => {
            spans.push(Span::styled("* * *", Style::default().fg(rgb(theme.muted))));
            return Line::from(spans);
        }
        LineKind::Blank => return Line::from(""),
        LineKind::Quote => spans.push(Span::styled("▏ ", Style::default().fg(rgb(theme.muted)))),
        // A comment is not book text: it is painted in its annotation's colour
        // so the two belong together at a glance.
        LineKind::Note { color: (r, g, b) } => {
            for piece in &source.pieces {
                spans.push(Span::styled(
                    piece.text.clone(),
                    Style::default()
                        .fg(Color::Rgb(r, g, b))
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            return Line::from(spans);
        }
        _ => {}
    }

    // Counts only real block text, so decoration never shifts an offset.
    let mut column = 0usize;
    for piece in &source.pieces {
        let base = base_style(piece.style, source.kind, theme);
        if piece.decoration {
            spans.push(Span::styled(piece.text.clone(), base));
            continue;
        }
        // Split the piece wherever a highlight, the selection or the cursor
        // starts or ends.
        let mut current: Option<(CellStyle, String)> = None;
        for ch in piece.text.chars() {
            let cell = cell_style_at(
                source.block,
                source.offset + column,
                line_index,
                column,
                marks,
                selection,
                cursor,
                search,
            );
            match &mut current {
                Some((style, text)) if *style == cell => text.push(ch),
                Some((style, text)) => {
                    spans.push(styled_span(std::mem::take(text), base, *style, theme));
                    current = Some((cell, ch.to_string()));
                }
                None => current = Some((cell, ch.to_string())),
            }
            column += 1;
        }
        if let Some((style, text)) = current {
            spans.push(styled_span(text, base, style, theme));
        }
    }

    // A listing's backdrop runs to the right edge, so the block is a rectangle
    // rather than a ragged shape that follows the text.
    if source.kind == LineKind::Code {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        if (used as u16) < width {
            spans.push(Span::styled(
                " ".repeat(width as usize - used),
                Style::default().bg(rgb(theme.code_background)),
            ));
        }
    }
    Line::from(spans)
}

/// Paints one mark per line that carries an annotation, in the same visual
/// language as the passage itself: coloured for a plain highlight, inverted once
/// a comment is attached.
fn draw_gutter(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let marks = app.annotations_here();
    if marks.is_empty() {
        return;
    }

    let lines: Vec<Line> = app
        .visible_lines()
        .iter()
        .map(|line| {
            let len = line.text_len();
            let found = marks.iter().find(|mark| {
                (0..len.max(1)).any(|column| mark.covers(line.block, line.offset + column))
            });
            match found {
                Some(mark) => Line::from(mark_span(
                    theme.marks[mark.color.index()],
                    mark.has_note(),
                    "▌",
                )),
                None => Line::from(" "),
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The mark that stands for an annotation, wherever one is shown.
///
/// A commented annotation is inverted rather than coloured, so mark and passage
/// always match. Inverting a blank fills the cell with the terminal's own text
/// colour, which reads in any theme.
fn mark_span(color: crate::theme::Rgb, has_note: bool, glyph: &'static str) -> Span<'static> {
    let (r, g, b) = color;
    if has_note {
        Span::styled(
            " ".repeat(glyph.chars().count()),
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .underline_color(Color::Rgb(r, g, b)),
        )
    } else {
        Span::styled(glyph, Style::default().fg(Color::Rgb(r, g, b)))
    }
}

fn cell_style_at(
    block: usize,
    offset: usize,
    line: usize,
    column: usize,
    marks: &[&Annotation],
    selection: Option<((usize, usize), (usize, usize))>,
    cursor: Option<(usize, usize)>,
    search: &crate::search::Search,
) -> CellStyle {
    let mark = marks.iter().find(|a| a.covers(block, offset));
    let selected = selection.is_some_and(|(start, end)| {
        let here = (block, offset);
        here >= start && here <= end
    });
    // A match spans the length of the query from its start.
    let length = search.len();
    let hit = search.hits().iter().rev().find(|(hit_block, start)| {
        *hit_block == block && offset >= *start && offset < start + length
    });
    CellStyle {
        mark: mark.map(|a| a.color),
        selected,
        at_cursor: cursor == Some((line, column)),
        has_note: mark.is_some_and(|a| a.has_note()),
        match_here: hit.is_some(),
        current_match: hit.is_some_and(|(b, start)| search.is_current(*b, *start)),
    }
}

fn base_style(piece: crate::doc::RunStyle, kind: LineKind, theme: &crate::theme::Theme) -> Style {
    let mut style = Style::default();
    if piece.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if piece.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if piece.link {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if piece.code {
        style = style.fg(rgb(theme.code_foreground));
    }
    match kind {
        LineKind::Heading(1 | 2) => style.add_modifier(Modifier::BOLD).fg(rgb(theme.accent)),
        LineKind::Heading(_) => style.fg(rgb(theme.accent)),
        LineKind::Quote => style.fg(rgb(theme.quote)).add_modifier(Modifier::ITALIC),
        // A listing is set apart by a background of its own rather than by
        // colour alone, so it reads as one block even where lines are short.
        LineKind::Code => style
            .fg(rgb(theme.code_foreground))
            .bg(rgb(theme.code_background)),
        LineKind::Image { .. } => style.fg(rgb(theme.muted)).add_modifier(Modifier::ITALIC),
        _ => style,
    }
}

fn styled_span(
    text: String,
    base: Style,
    cell: CellStyle,
    theme: &crate::theme::Theme,
) -> Span<'static> {
    let mut style = base;

    if let Some(color) = cell.mark {
        let (r, g, b) = theme.marks[color.index()];
        // The comment decides how a passage is marked, not how the annotation
        // came about. An annotation that carries text is inverted; one that only
        // colours the passage gets the colour. That way the two never look alike,
        // and it holds for annotations made before this rule existed.
        if cell.has_note {
            // Inverting uses the terminal's own colours, so it reads in any
            // theme. The annotation's colour stays visible as the underline, in
            // the margin, and on the comment line itself.
            style = style
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(Color::Rgb(r, g, b));
        } else {
            let (fr, fg_, fb) = text_color_on((r, g, b));
            // A terminal has no transparency, so both colours are set. That keeps
            // the text readable whatever the terminal background is.
            style = style.bg(Color::Rgb(r, g, b)).fg(Color::Rgb(fr, fg_, fb));
        }
    }
    // A match outranks a highlight: it is what the reader is looking for right
    // now, while a highlight is a standing note. The one match the reader is on
    // is inverted on top, so it stands out among its siblings.
    if cell.match_here {
        let (r, g, b) = theme.accent;
        let (fr, fg_, fb) = text_color_on((r, g, b));
        style = style.bg(Color::Rgb(r, g, b)).fg(Color::Rgb(fr, fg_, fb));
        if cell.current_match {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
    }
    if cell.selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if cell.at_cursor {
        style = style
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD);
    }
    Span::styled(text, style)
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();

    // While a search is being typed, the status line is the prompt.
    if let Some(input) = app.search_input() {
        let line = Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled("/", Style::default().fg(rgb(theme.accent))),
            Span::styled(
                input.to_string(),
                Style::default().fg(rgb(theme.foreground)),
            ),
            // A block marks where the next character lands.
            Span::styled("▏", Style::default().fg(rgb(theme.accent))),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let (current, total) = app.chapter_number();

    // What the cursor stands on outranks the title: it says whether there is an
    // annotation here and what its comment says.
    let left = match app.status() {
        Some(message) => message.to_string(),
        // A link under the cursor is what the reader would act on next, so it
        // outranks the annotation note.
        None if app.link_at_cursor().is_some() => {
            let link = app.link_at_cursor().expect("checked above");
            format!("link: {}  ·  Enter follows", link.target)
        }
        None => match app.annotation_at_cursor() {
            Some(mark) => match &mark.note {
                Some(note) => format!("{}: {}", mark.color.label(), note),
                None => format!(
                    "{} highlight, no comment - e writes one",
                    mark.color.label()
                ),
            },
            None => format!("{}  ·  {}", app.title(), app.chapter_title()),
        },
    };

    let mode = match app.mode {
        Mode::Normal => "NORMAL  ",
        Mode::Visual => "VISUAL  ",
        _ => "",
    };
    let marks = app.annotations_here().len();
    let counter = match marks {
        0 => String::new(),
        1 => "1 mark  ".to_string(),
        n => format!("{n} marks  "),
    };
    let right = format!("{mode}{counter}{current}/{total}  {:>3}%", app.progress());

    let gap = area
        .width
        .saturating_sub(
            left.chars().count() as u16 + right.chars().count() as u16 + SIDE_MARGIN * 2,
        )
        .max(1);

    let mode_style = match app.mode {
        Mode::Visual => Style::default().fg(rgb(theme.accent)),
        Mode::Normal => Style::default().fg(rgb(theme.marks[1])),
        _ => Style::default().fg(rgb(theme.muted)),
    };

    let line = Line::from(vec![
        Span::raw(" ".repeat(SIDE_MARGIN as usize)),
        Span::styled(left, Style::default().fg(rgb(theme.muted))),
        Span::raw(" ".repeat(gap as usize)),
        Span::styled(right, mode_style),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn draw_contents(frame: &mut Frame, area: Rect, app: &App) {
    let entries = app.contents();
    let panel = centred(area, 60, entries.len() as u16 + 2);
    let items: Vec<ListItem> = entries.into_iter().map(ListItem::new).collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Contents ")
                .border_style(Style::default().fg(rgb(app.theme().muted))),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    state.select(Some(app.contents_cursor()));
    frame.render_widget(Clear, panel);
    frame.render_stateful_widget(list, panel, &mut state);
}

fn draw_annotations(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let annotations = app.annotations();
    let panel = centred(area, 78, annotations.len() as u16 * 2 + 2);

    let items: Vec<ListItem> = annotations
        .iter()
        .map(|annotation| {
            let marker = mark_span(
                theme.marks[annotation.color.index()],
                annotation.has_note(),
                "  ",
            );
            let quote = shorten(&annotation.quote, panel.width.saturating_sub(8) as usize);
            let mut lines = vec![Line::from(vec![marker, Span::raw(" "), Span::raw(quote)])];
            if let Some(note) = &annotation.note {
                lines.push(Line::from(Span::styled(
                    format!(
                        "     {}",
                        shorten(note, panel.width.saturating_sub(8) as usize)
                    ),
                    Style::default().fg(rgb(theme.muted)),
                )));
            }
            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Annotations - Enter jumps, e comments, d deletes, m+colour recolours ")
                .border_style(Style::default().fg(rgb(theme.muted))),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    state.select(Some(app.annotations_cursor()));
    frame.render_widget(Clear, panel);
    frame.render_stateful_widget(list, panel, &mut state);
}

/// Draws the key bindings, grouped as they are declared.
fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme();
    let mut lines: Vec<Line> = Vec::new();
    for (group, bindings) in crate::app::BINDINGS {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            *group,
            Style::default()
                .fg(rgb(theme.accent))
                .add_modifier(Modifier::BOLD),
        )));
        for (keys, what) in *bindings {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {keys:<20}"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(*what, Style::default().fg(rgb(theme.muted))),
            ]));
        }
    }

    let height = lines.len() as u16 + 2;
    let panel = centred(area, 66, height);
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Keys - any key closes ")
            .border_style(Style::default().fg(rgb(theme.muted))),
    );
    frame.render_widget(Clear, panel);
    frame.render_widget(paragraph, panel);
}

fn shorten(text: &str, width: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= width {
        return collapsed;
    }
    collapsed
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

/// Kept for the help overlay that will follow.
#[allow(dead_code)]
fn wrapped(text: &str) -> Paragraph<'_> {
    Paragraph::new(text).wrap(Wrap { trim: true })
}

// ----- the library view -----

/// Draws the shelf: one row per book, with a filter prompt when one is being
/// typed.
pub fn draw_shelf(frame: &mut Frame, shelf: &mut crate::shelf::Shelf, theme: &crate::theme::Theme) {
    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
    let (list_area, status_area) = (areas[0], areas[1]);

    let inner = Rect {
        x: list_area.x + SIDE_MARGIN,
        y: list_area.y,
        width: list_area.width.saturating_sub(SIDE_MARGIN * 2),
        height: list_area.height,
    };
    shelf.prepare(inner.height);

    let width = inner.width as usize;
    // Author and series get fixed shares; the title takes what is left, because
    // it is what the eye looks for first.
    let author_width = (width / 4).clamp(12, 30);
    let series_width = (width / 5).clamp(0, 24);
    let title_width = width.saturating_sub(author_width + series_width + 6);

    let scroll = shelf.scroll();
    let rows: Vec<Line> = shelf
        .entries()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner.height as usize)
        .map(|(index, entry)| {
            let selected = index == shelf.cursor();
            let record = &entry.record;

            // A book that has been opened before carries a mark, so picking up
            // where you left off does not need remembering.
            let started = if entry.progress.is_some() { "▌" } else { " " };
            // Sorted by when it was read, the date is what the column is for.
            let third = if shelf.order() == crate::library::Order::Recent {
                entry.last_read.map(when).unwrap_or_default()
            } else {
                match (&record.series, record.series_index) {
                    (Some(name), Some(at)) => format!("{name} {at}"),
                    (Some(name), None) => name.clone(),
                    _ => String::new(),
                }
            };

            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let dim = if selected {
                base
            } else {
                base.fg(rgb(theme.muted))
            };

            let mut spans = vec![
                Span::styled(started.to_string(), base.fg(rgb(theme.accent))),
                Span::styled(" ", base),
                Span::styled(pad(&record.display_title(), title_width), base),
                Span::styled("  ", base),
                Span::styled(pad(&record.display_authors(), author_width), dim),
            ];
            if series_width > 0 {
                spans.push(Span::styled("  ", base));
                spans.push(Span::styled(pad(&third, series_width), dim));
            }
            if record.missing {
                spans.push(Span::styled(" (missing)", base.fg(rgb(theme.marks[3]))));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(rows), inner);
    draw_shelf_status(frame, status_area, shelf, theme);

    if shelf.mode == crate::shelf::Mode::Help {
        draw_shelf_help(frame, list_area, theme);
    }
}

fn draw_shelf_status(
    frame: &mut Frame,
    area: Rect,
    shelf: &crate::shelf::Shelf,
    theme: &crate::theme::Theme,
) {
    if let Some(input) = shelf.filter_input() {
        let line = Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled("/", Style::default().fg(rgb(theme.accent))),
            Span::styled(
                input.to_string(),
                Style::default().fg(rgb(theme.foreground)),
            ),
            Span::styled("▏", Style::default().fg(rgb(theme.accent))),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    let shown = shelf.entries().len();
    let total = shelf.total();
    let left = match shelf.status() {
        Some(message) => message.to_string(),
        None if !shelf.filter().is_empty() => {
            format!("{shown} of {total} books  ·  filter {:?}", shelf.filter())
        }
        None => format!("{total} books"),
    };
    let right = format!("by {}  ·  ? for keys", shelf.order().label());
    let gap = area
        .width
        .saturating_sub(
            left.chars().count() as u16 + right.chars().count() as u16 + SIDE_MARGIN * 2,
        )
        .max(1);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled(left, Style::default().fg(rgb(theme.muted))),
            Span::raw(" ".repeat(gap as usize)),
            Span::styled(right, Style::default().fg(rgb(theme.muted))),
        ])),
        area,
    );
}

fn draw_shelf_help(frame: &mut Frame, area: Rect, theme: &crate::theme::Theme) {
    let mut lines = vec![Line::from(Span::styled(
        "Library",
        Style::default()
            .fg(rgb(theme.accent))
            .add_modifier(Modifier::BOLD),
    ))];
    for (keys, what) in crate::shelf::Shelf::BINDINGS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {keys:<20}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(*what, Style::default().fg(rgb(theme.muted))),
        ]));
    }
    let panel = centred(area, 62, lines.len() as u16 + 2);
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Keys - any key closes ")
                .border_style(Style::default().fg(rgb(theme.muted))),
        ),
        panel,
    );
}

/// How long ago something happened, in a form that fits a narrow column.
fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    let elapsed = chrono::Utc::now().signed_duration_since(at);
    let days = elapsed.num_days();
    match days {
        d if d < 0 => "just now".into(),
        0 => {
            let hours = elapsed.num_hours();
            if hours < 1 {
                format!("{} min ago", elapsed.num_minutes().max(1))
            } else {
                format!("{hours} h ago")
            }
        }
        1 => "yesterday".into(),
        d if d < 30 => format!("{d} days ago"),
        d if d < 365 => format!("{} months ago", d / 30),
        d => format!("{} years ago", d / 365),
    }
}

/// Cuts or pads a value to a fixed width, so the columns line up.
fn pad(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        format!("{text:<width$}")
    }
}

// ----- the hit list -----

/// Draws search hits: book, chapter and the passage that matched.
pub fn draw_hits(
    frame: &mut Frame,
    results: &crate::find::Results,
    cursor: usize,
    scroll: usize,
    theme: &crate::theme::Theme,
) {
    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
    let (list_area, status_area) = (areas[0], areas[1]);
    let inner = Rect {
        x: list_area.x + SIDE_MARGIN,
        y: list_area.y,
        width: list_area.width.saturating_sub(SIDE_MARGIN * 2),
        height: list_area.height,
    };

    // Two rows per hit: where it is, and what it says. One row would force the
    // passage to compete with the title for the same width.
    let mut lines: Vec<Line> = Vec::new();
    for (index, hit) in results.hits.iter().enumerate().skip(scroll) {
        if lines.len() + 3 > inner.height as usize {
            break;
        }
        let selected = index == cursor;
        let heading = if selected {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▌ " } else { "  " },
                heading.fg(rgb(theme.accent)),
            ),
            Span::styled(hit.book_title.clone(), heading),
            Span::styled(
                format!("  ·  {}", hit.chapter_title),
                if selected {
                    heading
                } else {
                    Style::default().fg(rgb(theme.muted))
                },
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "    {}",
                shorten(&hit.snippet, inner.width.saturating_sub(6) as usize)
            ),
            Style::default().fg(rgb(theme.muted)),
        )));
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  nothing found for {:?}", results.query),
            Style::default().fg(rgb(theme.muted)),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);

    let source = match results.source {
        crate::find::Source::Index => "qmd index",
        crate::find::Source::Direct => "read directly",
    };
    let left = format!(
        "{} hits for {:?}  ·  {source}",
        results.hits.len(),
        results.query
    );
    let right = "Enter opens  ·  q leaves".to_string();
    let gap = status_area
        .width
        .saturating_sub(
            left.chars().count() as u16 + right.chars().count() as u16 + SIDE_MARGIN * 2,
        )
        .max(1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(SIDE_MARGIN as usize)),
            Span::styled(left, Style::default().fg(rgb(theme.muted))),
            Span::raw(" ".repeat(gap as usize)),
            Span::styled(right, Style::default().fg(rgb(theme.muted))),
        ])),
        status_area,
    );
}
