//! Where the lines go.
//!
//! The thin half of the renderer: `draw.rs` says what the screen *says* and this
//! puts it into areas. Separated because a renderer written as one function is
//! only assertable by drawing it into a buffer and reading pixels back, and
//! nearly every question worth asking about this interface is about wording.
//!
//! # Separation is a background, not a line
//!
//! The session pane sits on a slightly raised background and the rail sits on
//! the terminal's own. That is the window's one structural device: no borders,
//! no rules, no dividers. Two columns of prose separated by a `│` read as a
//! table, and a table is the wrong shape for a conversation.
//!
//! It degrades honestly. `Theme::surface` is empty below 256 colours — there is
//! no "slightly lighter" in the original sixteen, and inventing one out of a
//! reversed cell would make the pane the *loudest* thing on screen — so a
//! terminal that cannot raise a background gets a muted rule in the gutter
//! instead. The layout does not change; only what fills it does.
//!
//! # A margin, and a line of nothing above everything
//!
//! Two columns down each edge and one blank row at the top, both on the
//! terminal's own background. A full-bleed frame reads as a program that has
//! taken the screen; an inset one reads as a window on it, which is what this
//! is — the shell it was started from is one Ctrl-C away and comes back intact.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use riabuild_theme::{Role, Theme};

use crate::app::{App, Focus};
use crate::draw::{self, Chrome};

/// The margin down each edge of the window, and inside the pane.
const MARGIN: u16 = 2;

/// The columns between the rail and the pane's raised background.
///
/// One rather than two, and that is what a developer counting the gap sees as
/// **two**: `draw::rail_lines` already keeps a column spare at the rail's own
/// right edge, so the blank between the last glyph of a session's title and the
/// first cell of the pane is this plus that. Three read as a seam rather than a
/// join. The rail's own spare column is what stops a long title touching the
/// pane, which is why the width came off here instead of there.
const GUTTER: u16 = 1;

/// `area` with `MARGIN` columns taken off each side.
fn inset(area: Rect) -> Rect {
    let width = area.width.saturating_sub(MARGIN * 2);
    Rect {
        x: area.x + MARGIN.min(area.width),
        y: area.y,
        width,
        height: area.height,
    }
}

/// Puts the lines into blocks.
pub fn render(frame: &mut Frame, app: &App, chrome: Chrome<'_>) {
    let area = frame.area();
    // Ratatui writes only the cells that differ from the frame before, so a
    // shorter line leaves the tail of a longer one behind it. Clearing first is
    // cheap at terminal sizes and removes a whole class of ghost.
    frame.render_widget(Clear, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // the blank row at the top
        Constraint::Length(1), // the window's name and this repository
        Constraint::Length(1),
        Constraint::Min(1), // the rail and the pane
        Constraint::Length(1),
        Constraint::Length(1), // the key hints
    ])
    .split(area);

    let head = inset(rows[1]);
    frame.render_widget(Paragraph::new(draw::header_line(chrome)), head);
    frame.render_widget(
        Paragraph::new(draw::counts_line(app, chrome.theme)).alignment(Alignment::Right),
        head,
    );

    let body = inset(rows[3]);
    let rail_width = draw::rail_width(body.width);
    let split = Layout::horizontal([
        Constraint::Length(rail_width),
        Constraint::Length(GUTTER),
        Constraint::Min(1),
    ])
    .split(body);
    render_rail(frame, app, chrome, rail_width, split[0]);
    render_pane(frame, app, chrome, split[1], split[2]);

    frame.render_widget(
        Paragraph::new(draw::footer_line(app, chrome.theme, inset(rows[5]).width)),
        inset(rows[5]),
    );

    // Last, and over the body: it is a question, so it covers what it was asked
    // from rather than taking a column away from it.
    if app.focus == Focus::Picker {
        render_picker(frame, app, chrome, rows[3]);
    }
}

/// The rail, scrolled so the row under the cursor is on screen.
///
/// It needed no scrolling while a session was one line: ten of them fitted in
/// the body of a laptop terminal. Two lines each halves that, and a cursor the
/// developer cannot see is worse than a rail that does not show everything — so
/// the list follows the cursor, the way the chooser already does.
///
/// Both of a row's lines are kept in view rather than just its first, because
/// the second is where a long title finishes and where `(subagent)` is said.
fn render_rail(frame: &mut Frame, app: &App, chrome: Chrome<'_>, width: u16, area: Rect) {
    let lines = draw::rail_lines(app, chrome, width);
    let cursor = draw::rail_cursor_line(app) as u16;
    let offset = (cursor + draw::ROW_LINES as u16).saturating_sub(area.height);
    // Never past the end: scrolling a short list off the top would leave the
    // rail blank on a window that had room for all of it.
    let last = (lines.len() as u16).saturating_sub(area.height);
    frame.render_widget(Paragraph::new(lines).scroll((offset.min(last), 0)), area);
}

/// The session pane, on its own background.
///
/// `gutter` is the two columns between it and the rail, and it is only drawn on
/// where the terminal cannot raise a background — there the rule stands in for
/// the surface that would otherwise separate the two.
fn render_pane(frame: &mut Frame, app: &App, chrome: Chrome<'_>, gutter: Rect, area: Rect) {
    let theme = chrome.theme;
    if theme.has_surfaces() {
        frame.render_widget(Block::default().style(theme.surface()), area);
    } else {
        frame.render_widget(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(theme.style(Role::Muted)),
            gutter,
        );
    }

    // The box takes as many rows as the prompt in it needs, and the transcript
    // gives them up. A cap, because a developer who pastes a page must not be
    // left with a conversation they cannot see — past it the box scrolls itself,
    // which `render_compose` does by dropping the rows above the caret.
    let inner_width = inset(area).width;
    let wanted =
        app.compose
            .height((inner_width as usize).saturating_sub(draw::COMPOSE_INDENT)) as u16;
    let room = area.height.saturating_sub(6).max(1);
    let box_height = wanted.clamp(1, room.min(BOX_LINES));

    let rows = Layout::vertical([
        Constraint::Length(1), // a blank row inside the pane's own edge
        Constraint::Length(1), // whose sign-in this is
        Constraint::Length(1),
        Constraint::Min(1),    // the conversation, or what would start one
        Constraint::Length(1), // the newline above the box
        Constraint::Length(box_height), // the box
        Constraint::Length(1),
    ])
    .split(area);

    let status = inset(rows[1]);
    paint(
        frame,
        theme,
        Paragraph::new(draw::status_line(app, theme, status.width)),
        status,
    );

    let middle = inset(rows[3]);
    match app.selected() {
        Some(_) => render_transcript(frame, app, chrome, middle),
        // Not a transcript at all: nothing has been asked, so the only thing
        // worth saying is what typing here would start.
        None => render_splash(frame, app, theme, middle),
    }

    render_compose(frame, app, theme, inset(rows[5]));
}

/// The tallest the prompt box may grow before it starts scrolling itself.
///
/// A developer who pastes a page of logs into it must still be able to see the
/// conversation they are pasting them into.
const BOX_LINES: u16 = 8;

/// The box, with the caret's row always in it.
///
/// Scrolled from the bottom rather than the top: the caret is where the typing
/// is happening, so a prompt taller than the box shows its end, and moving the
/// caret back up brings the earlier rows with it.
fn render_compose(frame: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let lines = draw::compose_lines(app, theme, area.width);
    let caret = app
        .compose
        .wrap((area.width as usize).saturating_sub(draw::COMPOSE_INDENT))
        .caret
        .0 as u16;
    let offset = (caret + 1).saturating_sub(area.height);
    paint(
        frame,
        theme,
        Paragraph::new(lines).scroll((offset, 0)),
        area,
    );
}

/// Renders a paragraph on the pane's background rather than on the terminal's.
///
/// `Theme::surface` sets a foreground beside the background, and a span that
/// names only a colour keeps it — which is what makes prose readable on the
/// raised pane without every call site restating the pair.
fn paint(frame: &mut Frame, theme: Theme, paragraph: Paragraph<'_>, area: Rect) {
    frame.render_widget(paragraph.style(theme.surface()), area);
}

fn render_transcript(frame: &mut Frame, app: &App, chrome: Chrome<'_>, area: Rect) {
    let lines = draw::transcript_lines(app.selected(), chrome.theme, chrome.unicode);
    // Follow the newest output unless the developer has scrolled up. Ratatui's
    // `scroll` counts from the top, so "stick to the bottom" has to be computed
    // from the height every frame — there is no follow mode to switch on.
    let overflow = (lines.len() as u16).saturating_sub(area.height);
    let offset = overflow.saturating_sub(app.scrollback);
    paint(
        frame,
        chrome.theme,
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

/// What an offer shows instead of a conversation: a sentence in the middle.
fn render_splash(frame: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(account) = app.offered() else {
        return;
    };
    let email = app.login_of(account.kind, account.number);
    let signed_out = app.is_signed_out(account.kind, account.number);
    let lines: Vec<Line<'static>> = draw::splash_lines(account, email, signed_out, theme);
    // Wrapped, and measured *after* wrapping. The sentence a signed-out sign-in
    // shows is a whole one — it names the account and the command that fixes it
    // — and it is longer than a pane beside a rail on a laptop. Unwrapped it was
    // simply cut at the edge, which took the command off the end of the only
    // line that was any use.
    let height: u16 = lines
        .iter()
        .map(|line| wrapped_height(line, area.width))
        .sum();
    let middle = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: area.width,
        height: height.min(area.height),
    };
    paint(
        frame,
        theme,
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .alignment(Alignment::Center),
        middle,
    );
}

/// How many rows one line takes once it has been wrapped to `width`.
///
/// By character rather than by byte, because an em-dash is one column and three
/// bytes and a height computed from `len()` would leave a sentence with a row
/// too few — which is the same cut this exists to prevent, one row further down.
fn wrapped_height(line: &Line<'_>, width: u16) -> u16 {
    let columns: usize = line
        .spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum();
    let width = width.max(1) as usize;
    (columns.div_ceil(width)).max(1) as u16
}

/// The chooser, centred over the body.
fn render_picker(frame: &mut Frame, app: &App, chrome: Chrome<'_>, area: Rect) {
    let lines = draw::picker_lines(app, chrome.theme, chrome.unicode);
    let width = 44.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    // Without this the pane underneath shows through the gaps in the box, for
    // the reason the terminal's own history did before `claim` cleared it:
    // ratatui writes differences, and a cell a widget does not set is a cell
    // nobody wrote.
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .title(" new session ")
        .border_style(chrome.theme.style(Role::Brand));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    // Twenty-seven sign-ins do not fit in a box this size, so the list follows
    // the cursor rather than the cursor being limited to the box.
    let offset = (app.picking as u16 + 1).saturating_sub(inner.height.max(1));
    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;
    use crate::app::Pane;
    use crate::draw::tests::every_account;
    use riabuild_harness::{Kind, testing};
    use riabuild_theme::{Depth, Tone};

    fn chrome(theme: Theme) -> Chrome<'static> {
        Chrome {
            theme,
            unicode: true,
            repo: Some("Clubria/riabuild"),
        }
    }

    /// One frame, as a terminal of that size would receive it.
    fn frame_of(app: &App, theme: Theme, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, app, chrome(theme)))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect()
            })
            .collect()
    }

    fn painted(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let theme = Theme::with_depth_and_tone(Depth::TrueColor, Tone::Dark);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, app, chrome(theme)))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn with_a_session() -> App {
        let mut app = App::new(every_account());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        for event in testing::decode(Kind::Claude, testing::CLAUDE) {
            app.observe("s1", &event);
        }
        app
    }

    #[test]
    fn the_window_opens_on_the_rail_and_says_what_a_new_session_would_be() {
        // Both halves of the first frame a developer sees: the cursor is in the
        // rail, and the pane beside it is not pretending to be a conversation.
        let app = App::new(every_account());
        assert_eq!(app.focus, Focus::List);
        let screen = frame_of(&app, Theme::plain(), 100, 24);
        assert!(
            screen
                .iter()
                .any(|row| row.contains("create a Claude session")),
            "{screen:#?}"
        );
        assert!(
            screen.iter().any(|row| row.contains("0 sessions")),
            "{screen:#?}"
        );
        assert!(
            screen.iter().any(|row| row.contains("Clubria/riabuild")),
            "{screen:#?}"
        );
    }

    #[test]
    fn the_top_row_is_blank_and_both_edges_keep_two_columns() {
        let app = with_a_session();
        let screen = frame_of(&app, Theme::plain(), 100, 24);
        assert!(screen[0].trim().is_empty(), "{:?}", screen[0]);
        for row in &screen {
            let chars: Vec<char> = row.chars().collect();
            assert_eq!(&chars[..2], &[' ', ' '], "{row:?}");
            assert_eq!(&chars[chars.len() - 2..], &[' ', ' '], "{row:?}");
        }
    }

    #[test]
    fn the_pane_is_a_background_rather_than_a_line_beside_the_rail() {
        // The redesign's one structural device. A `│` between two columns of
        // prose reads as a table, and a conversation is not one.
        let app = with_a_session();
        let buffer = painted(&app, 100, 24);
        let theme = Theme::with_depth_and_tone(Depth::TrueColor, Tone::Dark);
        let surface = theme.surface().bg;
        assert!(surface.is_some());
        // The rail is on the terminal's own background and the pane is not.
        assert_eq!(buffer[(4, 10)].bg, ratatui::style::Color::Reset);
        assert_eq!(Some(buffer[(60, 10)].bg), surface);
        // and the top row is the terminal's, all the way across
        for column in 0..100 {
            assert_eq!(buffer[(column, 0)].bg, ratatui::style::Color::Reset);
        }
    }

    #[test]
    fn a_terminal_with_no_raised_background_gets_a_rule_instead() {
        // There is no "slightly lighter" in the original sixteen. Faking one
        // would make the pane the loudest thing on screen.
        let sixteen = Theme::with_depth_and_tone(Depth::Ansi16, Tone::Dark);
        assert!(!sixteen.has_surfaces());
        let app = with_a_session();
        let screen = frame_of(&app, sixteen, 100, 24);
        assert!(
            screen.iter().any(|row| row.contains('│')),
            "no rule where there is no surface\n{screen:#?}"
        );
    }

    #[test]
    fn two_columns_separate_the_rail_from_the_pane_and_no_more() {
        // Three read as a seam rather than a join. The gap is the rail's own
        // spare column plus `GUTTER`, so this asserts what a developer counts
        // rather than either constant on its own.
        let mut app = App::new(every_account());
        app.add(Pane::new(
            "s1".into(),
            Kind::Claude,
            // Long enough to fill the rail's title column, which is the only
            // row that can prove where the rail's text actually stops.
            "a title long enough to run the whole way along the rail".into(),
        ));
        app.cursor = 0;

        let buffer = painted(&app, 100, 24);
        let theme = Theme::with_depth_and_tone(Depth::TrueColor, Tone::Dark);
        let surface = theme.surface().bg.expect("a raised pane");
        let row = 1 + 1 + 1 + 1; // the top blank, the header, its gap, the rail's first line
        let pane_at = (0..100)
            .find(|column| buffer[(*column, row)].bg == surface)
            .expect("the pane is on screen");
        let last_glyph = (0..pane_at)
            .rfind(|column| buffer[(*column, row)].symbol() != " ")
            .expect("the rail has text on it");
        assert_eq!(
            pane_at - last_glyph - 1,
            2,
            "the gap between the rail and the pane is not two columns"
        );
    }

    #[test]
    fn a_prompt_longer_than_the_pane_wraps_instead_of_running_off_it() {
        // The bug: a single-line box took the caret off the right edge with it,
        // so the half of a paragraph a developer had just written was somewhere
        // they could not see.
        let mut app = with_a_session();
        app.focus = Focus::Session;
        let prompt = "why is the nightly job so slow when the cache is warm and nothing else \
                      on the box is doing anything at all";
        for ch in prompt.chars() {
            app.compose.insert(ch);
        }
        let screen = frame_of(&app, Theme::plain(), 100, 24);

        let box_rows: Vec<&String> = screen
            .iter()
            .filter(|row| row.contains("why is the nightly") || row.contains("on the box is doing"))
            .collect();
        assert_eq!(box_rows.len(), 2, "{screen:#?}");
        // The caret is on the last of them, which is where the typing is.
        assert!(
            screen
                .iter()
                .any(|row| row.contains("anything at all") && row.contains('▏')),
            "{screen:#?}"
        );
        // Every row of it stays inside the window's own margin.
        for row in &screen {
            assert_eq!(row.chars().count(), 100, "{row:?}");
        }
    }

    #[test]
    fn the_box_lives_inside_the_pane_and_not_across_the_window() {
        let mut app = with_a_session();
        app.focus = Focus::Session;
        for ch in "why is it slow".chars() {
            app.compose.insert(ch);
        }
        let screen = frame_of(&app, Theme::plain(), 100, 24);
        let row = screen
            .iter()
            .find(|row| row.contains("why is it slow"))
            .expect("the box is on screen");
        // It starts inside the pane rather than at the window's own margin, and
        // there is a blank row under it.
        let at = row.find('›').expect("the box's mark");
        let rail = draw::rail_width(96);
        assert!(at as u16 > rail, "{at} is not inside the pane\n{row:?}");
    }

    #[test]
    fn the_chooser_covers_the_pane_rather_than_showing_through_it() {
        let mut app = with_a_session();
        app.open_picker();
        let screen = frame_of(&app, Theme::plain(), 100, 24);
        assert!(
            screen.iter().any(|row| row.contains("new session")),
            "{screen:#?}"
        );
        let boxed: Vec<&String> = screen.iter().filter(|row| row.contains('│')).collect();
        assert!(boxed.len() > 5, "{screen:#?}");
        for row in &boxed {
            let inside = row.split('│').nth(1).unwrap_or_default();
            assert!(
                ["claude-", "codex-", "grok-"]
                    .iter()
                    .any(|name| inside.contains(name)),
                "{inside:?} showed through the box\n{screen:#?}"
            );
        }
    }

    #[test]
    fn a_window_too_small_for_any_of_it_still_draws() {
        // A split terminal on a laptop. Every dimension here is arithmetic on
        // an area that can be smaller than the thing it is centring.
        let mut app = with_a_session();
        for (width, height) in [(100, 24), (40, 10), (20, 6), (8, 4), (4, 2), (1, 1)] {
            let _ = frame_of(&app, Theme::plain(), width, height);
        }
        app.open_picker();
        for (width, height) in [(100, 24), (30, 8), (12, 6), (4, 4)] {
            let _ = frame_of(&app, Theme::plain(), width, height);
        }
    }
}
