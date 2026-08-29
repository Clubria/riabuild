//! The frame.
//!
//! Split in two on purpose. The `*_lines` functions turn [`App`] into styled
//! [`Line`]s and touch no widget, no area and no terminal, so what the screen
//! *says* is testable without a backend; [`render`] is the thin part that puts
//! those lines into blocks. A renderer written as one function would only be
//! assertable by drawing it into a buffer and reading pixels back.
//!
//! Every colour comes from a [`Role`] through [`Theme::style`], never from a
//! literal. That is the same rule the rest of riabuild follows, and it matters
//! more here rather than less: ratatui will happily send a 24-bit escape to a
//! sixteen-colour terminal, so `Theme::style` is what stands between the
//! palette and an SSH session that cannot render it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use riabuild_theme::{Role, Style, Theme};

use crate::app::{App, Entry, Focus, Pane, State};

/// The mark against a session's state, and the role that colours it.
fn state_style(state: State, theme: Theme) -> Style {
    theme.style(match state {
        State::Trouble => Role::Danger,
        State::Idle => Role::Ok,
        State::Busy => Role::Busy,
    })
}

/// The frames of the one spinner, for a session that is working.
///
/// Braille rather than the block glyphs `riabuild-ui` uses for its own status
/// line: this one turns in place inside a list row, where a glyph that changes
/// width would make the column jitter on every tick.
const SPINNER: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

/// The session column, in columns.
///
/// Wide enough for a sign-in and a few words of a title, and narrow enough that
/// the transcript — which is prose — keeps a readable measure on an 80-column
/// terminal.
const LIST_WIDTH: u16 = 28;

/// The widest sign-in there is: `claude-9` is eight, and one space after it.
const ACCOUNT_WIDTH: usize = 9;

/// What is left of a row for the title, after the border, the cursor, the state
/// mark and the sign-in.
const TITLE_WIDTH: usize = LIST_WIDTH as usize - 1 - 1 - 2 - ACCOUNT_WIDTH;

/// `text`, ending in an ellipsis where it did not fit.
///
/// The alternative is what ratatui does by itself, which is to stop drawing at
/// the edge: a title cut mid-word with no mark reads as the whole title, and the
/// two sessions it was meant to tell apart look identical.
fn clip(text: &str, width: usize) -> String {
    if width == 0 || text.chars().count() <= width {
        return text.to_string();
    }
    // By character and never by byte: a title is whatever the developer typed,
    // and slicing "…" or an accented letter down the middle is a panic.
    let cut = text
        .char_indices()
        .nth(width - 1)
        .map(|(at, _)| at)
        .unwrap_or(text.len());
    format!("{}…", &text[..cut])
}

/// The session column.
pub fn list_lines(app: &App, theme: Theme, unicode: bool) -> Vec<Line<'static>> {
    if app.panes.is_empty() {
        return vec![Line::from(Span::styled(
            "no sessions yet — press n",
            theme.style(Role::Muted),
        ))];
    }
    app.panes
        .iter()
        .enumerate()
        .map(|(index, pane)| {
            let selected = index == app.selected;
            let mark = if pane.state() == State::Busy && unicode {
                SPINNER[app.tick % SPINNER.len()]
            } else {
                pane.state().mark(unicode)
            };
            // The selected row is marked by a leading bar rather than by a
            // reversed background: a session list is read down its left edge,
            // and a full-width highlight fights the state colour that is the
            // one thing the row exists to show.
            let cursor = match (selected, unicode) {
                (true, true) => "▌",
                (true, false) => ">",
                (false, _) => " ",
            };
            Line::from(vec![
                Span::styled(cursor, theme.style(Role::Brand)),
                Span::styled(format!("{mark} "), state_style(pane.state(), theme)),
                // The sign-in leads, and at a fixed width, for two reasons. It
                // is what tells two panes on one harness apart before either has
                // been asked anything — and a title is any length, so anything
                // after one is the thing that falls off the edge of a
                // twenty-eight column list. It is the launcher's own name, so it
                // matches whatever the developer signed in with.
                Span::styled(
                    format!("{:<ACCOUNT_WIDTH$}", pane.account_name()),
                    theme.style(Role::Muted),
                ),
                Span::styled(
                    clip(&pane.label(), TITLE_WIDTH),
                    theme.style(if selected { Role::Strong } else { Role::Muted }),
                ),
            ])
        })
        .collect()
}

/// The selected session's transcript.
pub fn transcript_lines(pane: Option<&Pane>, theme: Theme, unicode: bool) -> Vec<Line<'static>> {
    let Some(pane) = pane else {
        return vec![Line::from(Span::styled(
            "Press n to start an agent.",
            theme.style(Role::Muted),
        ))];
    };
    let mut lines = Vec::new();
    for (index, entry) in pane.entries.iter().enumerate() {
        // A subagent's work is indented under the session that asked for it,
        // which is the only cross-provider structure any of the three reports.
        let indent = if pane.delegated.contains(&index) {
            if unicode { "  ↳ " } else { "  > " }
        } else {
            ""
        };
        match entry {
            Entry::Said(text) => {
                for row in text.lines() {
                    lines.push(Line::from(vec![
                        Span::raw(indent.to_string()),
                        Span::styled(row.to_string(), Style::default()),
                    ]));
                }
            }
            Entry::Thought(text) => {
                for row in text.lines() {
                    lines.push(Line::from(vec![
                        Span::raw(indent.to_string()),
                        Span::styled(row.to_string(), theme.style(Role::Muted)),
                    ]));
                }
            }
            Entry::Tool {
                name, detail, ok, ..
            } => {
                let (mark, role) = match ok {
                    None if unicode => ("◌", Role::Busy),
                    None => (".", Role::Busy),
                    Some(true) if unicode => ("✓", Role::Ok),
                    Some(true) => ("+", Role::Ok),
                    Some(false) if unicode => ("✗", Role::Danger),
                    Some(false) => ("!", Role::Danger),
                };
                let mut spans = vec![
                    Span::raw(indent.to_string()),
                    Span::styled(format!("{mark} "), theme.style(role)),
                    Span::styled(name.clone(), theme.style(Role::Strong)),
                ];
                if let Some(detail) = detail {
                    spans.push(Span::styled(
                        format!("  {detail}"),
                        theme.style(Role::Muted),
                    ));
                }
                lines.push(Line::from(spans));
            }
            Entry::Trouble(text) => {
                for row in text.lines() {
                    lines.push(Line::from(vec![
                        Span::raw(indent.to_string()),
                        Span::styled(row.to_string(), theme.style(Role::Danger)),
                    ]));
                }
            }
            Entry::Note(text) => lines.push(Line::from(Span::styled(
                text.clone(),
                theme.style(Role::Brand),
            ))),
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "waiting for the first reply…",
            theme.style(Role::Muted),
        )));
    }
    lines
}

/// The sign-ins a new session can be started under.
pub fn picker_lines(app: &App, theme: Theme, unicode: bool) -> Vec<Line<'static>> {
    if app.accounts.is_empty() {
        return vec![Line::from(Span::styled(
            "no accounts yet — run riabuild",
            theme.style(Role::Muted),
        ))];
    }
    app.accounts
        .all()
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let selected = index == app.picking;
            let cursor = match (selected, unicode) {
                (true, true) => "▌",
                (true, false) => ">",
                (false, _) => " ",
            };
            Line::from(vec![
                Span::styled(cursor, theme.style(Role::Brand)),
                Span::styled(
                    format!("{:<10}", account.name()),
                    theme.style(if selected { Role::Strong } else { Role::Muted }),
                ),
                Span::styled(account.kind.label(), theme.style(Role::Muted)),
            ])
        })
        .collect()
}

/// The one-line header.
pub fn header_line(app: &App, theme: Theme) -> Line<'static> {
    let busy = app.busy_count();
    let mut spans = vec![
        Span::styled("riabuild agents", theme.style(Role::Brand)),
        Span::styled(
            format!("  {} session{}", app.panes.len(), plural(app.panes.len())),
            theme.style(Role::Muted),
        ),
    ];
    if busy > 0 {
        spans.push(Span::styled(
            format!("  {busy} working"),
            theme.style(Role::Busy),
        ));
    }
    if let Some(pane) = app.selected()
        && (pane.input_tokens > 0 || pane.output_tokens > 0)
    {
        spans.push(Span::styled(
            format!(
                "  {} in / {} out",
                thousands(pane.input_tokens),
                thousands(pane.output_tokens)
            ),
            theme.style(Role::Muted),
        ));
    }
    Line::from(spans)
}

/// The key hints, which change with what the keyboard is talking to.
pub fn footer_line(app: &App, theme: Theme) -> Line<'static> {
    let keys: &[(&str, &str)] = match app.focus {
        Focus::Transcript => &[
            ("↑↓", "scroll"),
            ("←", "sessions"),
            ("enter", "write"),
            ("n", "new"),
            ("q", "quit"),
        ],
        Focus::Sessions => &[
            ("↑↓", "session"),
            ("→", "read"),
            ("n", "new"),
            ("q", "quit"),
        ],
        Focus::Compose => &[("enter", "send"), ("esc", "back")],
        Focus::Picker => &[("↑↓", "account"), ("enter", "open"), ("esc", "back")],
    };
    let mut spans = Vec::new();
    for (index, (key, what)) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", theme.style(Role::Muted)));
        }
        spans.push(Span::styled((*key).to_string(), theme.style(Role::Strong)));
        spans.push(Span::styled(format!(" {what}"), theme.style(Role::Muted)));
    }
    Line::from(spans)
}

/// The prompt box.
pub fn compose_line(app: &App, theme: Theme) -> Line<'static> {
    let writing = app.focus == Focus::Compose;
    let can_send = app.selected().is_some();
    if !can_send {
        return Line::from(Span::styled(
            "this session has ended",
            theme.style(Role::Muted),
        ));
    }
    let mut spans = vec![Span::styled("› ", theme.style(Role::Brand))];
    if writing {
        spans.push(Span::raw(app.composing.clone()));
        // A block rather than a real cursor: the terminal's own cursor would
        // have to be positioned, and every redraw would move it.
        spans.push(Span::styled("▏", theme.style(Role::Brand)));
    } else {
        spans.push(Span::styled(
            "press enter to write".to_string(),
            theme.style(Role::Muted),
        ));
    }
    Line::from(spans)
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Groups digits, because a token count is read as a magnitude.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Puts the lines above into blocks.
pub fn render(frame: &mut Frame, app: &App, theme: Theme, unicode: bool) {
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(3),    // body
        Constraint::Length(1), // compose
        Constraint::Length(1), // footer
    ])
    .split(area);

    frame.render_widget(Paragraph::new(header_line(app, theme)), rows[0]);

    let body =
        Layout::horizontal([Constraint::Length(LIST_WIDTH), Constraint::Min(20)]).split(rows[1]);
    render_list(frame, app, theme, unicode, body[0]);
    render_transcript(frame, app, theme, unicode, body[1]);

    frame.render_widget(Paragraph::new(compose_line(app, theme)), rows[2]);
    frame.render_widget(Paragraph::new(footer_line(app, theme)), rows[3]);

    // Last, and over the body: it is a question, so it covers what it was asked
    // from rather than taking a column away from it.
    if app.focus == Focus::Picker {
        render_picker(frame, app, theme, unicode, rows[1]);
    }
}

fn render_list(frame: &mut Frame, app: &App, theme: Theme, unicode: bool, area: Rect) {
    // The divider carries the focus. Up and down mean two different things
    // depending on which side of it the keyboard is talking to, so which side
    // that is has to be visible without reading the footer.
    let border = if app.focus == Focus::Sessions {
        Role::Brand
    } else {
        Role::Muted
    };
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(theme.style(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(list_lines(app, theme, unicode)), inner);
}

fn render_transcript(frame: &mut Frame, app: &App, theme: Theme, unicode: bool, area: Rect) {
    let lines = transcript_lines(app.selected(), theme, unicode);
    // Follow the newest output unless the developer has scrolled up. Ratatui's
    // `scroll` counts from the top, so "stick to the bottom" has to be computed
    // from the height every frame — there is no follow mode to switch on.
    let height = area.height.saturating_sub(1);
    let overflow = (lines.len() as u16).saturating_sub(height);
    let offset = overflow.saturating_sub(app.scrollback);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0))
            .block(Block::default().padding(ratatui::widgets::Padding::horizontal(1))),
        area,
    );
}

/// The chooser, centred over the body.
fn render_picker(frame: &mut Frame, app: &App, theme: Theme, unicode: bool, area: Rect) {
    let lines = picker_lines(app, theme, unicode);
    let width = 30.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    // Without this the transcript underneath shows through the gaps in the box,
    // for the reason the terminal's own history did before `claim` cleared it:
    // ratatui writes differences, and a cell a widget does not set is a cell
    // nobody wrote.
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .title(" new session ")
        .border_style(theme.style(Role::Brand));
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
    use crate::account::{Account, Accounts};
    use crate::app::Pane as TestPane;
    use riabuild_harness::{Kind, testing};
    use riabuild_theme::Depth;

    /// Every sign-in riabuild keeps, which is what the window is handed.
    fn every_account() -> Accounts {
        let mut all = Vec::new();
        for kind in Kind::ALL {
            for number in 1..=9 {
                all.push(Account::new(kind, number, None));
            }
        }
        Accounts::from(all)
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn played(kind: Kind, transcript: &str) -> App {
        let mut app = App::new(every_account());
        app.add(TestPane::new("s1".into(), kind, "the first prompt".into()));
        for event in testing::decode(kind, transcript) {
            app.observe("s1", &event);
        }
        app
    }

    #[test]
    fn a_real_session_renders_its_tool_call_and_its_answer() {
        let app = played(Kind::Claude, testing::CLAUDE);
        let theme = Theme::with_depth(Depth::TrueColor);
        let rendered: Vec<String> = transcript_lines(app.selected(), theme, true)
            .iter()
            .map(text_of)
            .collect();
        assert!(
            rendered.iter().any(|row| row.contains("Bash")
                && row.contains("cargo test --workspace")
                && row.starts_with('✓')),
            "{rendered:#?}"
        );
        assert!(rendered.iter().any(|row| row == "All tests pass."));
    }

    #[test]
    fn every_colour_on_screen_comes_from_the_palette() {
        // The rule the rest of riabuild follows, and the one ratatui makes easy
        // to break: a literal `Color::Rgb` here would reach a sixteen-colour
        // terminal as an escape it cannot read.
        let app = played(Kind::Claude, testing::CLAUDE);
        let sixteen = Theme::with_depth(Depth::Ansi16);
        let roles: Vec<Style> = [
            Role::Brand,
            Role::Ok,
            Role::Busy,
            Role::Warn,
            Role::Danger,
            Role::Muted,
            Role::Strong,
        ]
        .into_iter()
        .map(|role| sixteen.style(role))
        .collect();

        let mut lines = transcript_lines(app.selected(), sixteen, true);
        lines.extend(list_lines(&app, sixteen, true));
        lines.push(header_line(&app, sixteen));
        lines.push(footer_line(&app, sixteen));
        lines.push(compose_line(&app, sixteen));
        lines.extend(picker_lines(&app, sixteen, true));

        for line in &lines {
            for span in &line.spans {
                // Unstyled prose is fine — it is the terminal's own foreground.
                if span.style == Style::default() {
                    continue;
                }
                assert!(
                    roles.contains(&span.style),
                    "{:?} is not a role from the palette",
                    span.style
                );
            }
        }
    }

    #[test]
    fn no_colour_at_all_still_renders_every_line() {
        // `NO_COLOR`, or a terminal riabuild could not read. The screen has to
        // stay legible, which means the marks carry the meaning colour would.
        let app = played(Kind::Codex, testing::CODEX);
        let plain = Theme::plain();
        let lines = transcript_lines(app.selected(), plain, false);
        assert!(!lines.is_empty());
        for line in &lines {
            for span in &line.spans {
                assert_eq!(span.style, Style::default());
            }
        }
        // and the ASCII fallbacks are used rather than glyphs a dumb terminal
        // would render as boxes
        let list: String = list_lines(&app, plain, false).iter().map(text_of).collect();
        assert!(list.contains('!'), "{list}");
        assert!(!list.contains('▲'), "{list}");
    }

    #[test]
    fn a_delegated_line_is_indented_under_the_session_that_asked_for_it() {
        let mut app = App::new(Accounts::default());
        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        app.observe("s1", &riabuild_harness::Event::Said("mine".into()));
        app.observe(
            "s1",
            &riabuild_harness::Event::Delegated {
                parent: "toolu_1".into(),
                inner: Box::new(riabuild_harness::Event::Said("theirs".into())),
            },
        );
        let rendered: Vec<String> = transcript_lines(app.selected(), Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert_eq!(rendered, vec!["mine", "  ↳ theirs"]);
    }

    #[test]
    fn the_header_counts_sessions_and_says_how_many_are_working() {
        let mut app = App::new(Accounts::default());
        assert!(text_of(&header_line(&app, Theme::plain())).contains("0 sessions"));

        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        let text = text_of(&header_line(&app, Theme::plain()));
        // Singular, because "1 sessions" is the kind of detail that makes a
        // tool feel unfinished.
        assert!(text.contains("1 session"), "{text}");
        // Opening a session is not working. The window opens with three of
        // them and has asked none of them anything, so a header claiming three
        // are busy would be wrong on the very first frame.
        assert!(!text.contains("working"), "{text}");

        app.sent("do the thing");
        let busy = text_of(&header_line(&app, Theme::plain()));
        assert!(busy.contains("1 working"), "{busy}");
    }

    #[test]
    fn the_window_opens_one_pane_per_harness_riabuild_installs() {
        // No flag decides this. All three are installed, and the two that start
        // no process until they are spoken to cost nothing to have open.
        let mut app = App::new(Accounts::default());
        for (index, kind) in Kind::ALL.into_iter().enumerate() {
            app.add(TestPane::new(format!("s{index}"), kind, String::new()));
        }
        let rendered: Vec<String> = list_lines(&app, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert_eq!(rendered.len(), 3);
        for (row, kind) in rendered.iter().zip(Kind::ALL) {
            assert!(row.contains(kind.tag()), "{row}");
        }
    }

    #[test]
    fn a_title_too_long_for_its_column_ends_in_an_ellipsis() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("exactly-10", 10), "exactly-10");
        assert_eq!(clip("eleven-char", 10), "eleven-ch…");
        // By character and never by byte, or a title nobody wrote in ASCII is a
        // panic rather than a truncation.
        assert_eq!(clip("téléphone-ringing", 6), "télép…");
        assert_eq!(clip("anything", 0), "anything");
    }

    #[test]
    fn token_counts_are_grouped_so_a_magnitude_is_readable() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(28_431), "28,431");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn the_footer_says_what_the_keyboard_is_talking_to() {
        let mut app = App::new(Accounts::default());
        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        // Reading is the resting state, and the arrows scroll what is being
        // read. `pgup` is not advertised because it is not needed: a laptop
        // reaches it only as a chord, which is the whole reason this moved.
        let reading = text_of(&footer_line(&app, Theme::plain()));
        assert!(reading.contains("scroll"), "{reading}");
        assert!(reading.contains("sessions"), "{reading}");
        assert!(!reading.contains("pgup"), "{reading}");

        app.focus = Focus::Sessions;
        let picking = text_of(&footer_line(&app, Theme::plain()));
        assert!(picking.contains("session"), "{picking}");

        app.focus = Focus::Compose;
        let writing = text_of(&footer_line(&app, Theme::plain()));
        assert!(writing.contains("send"), "{writing}");
        // `n` must not be advertised while typing: it is a letter then.
        assert!(!writing.contains("new"), "{writing}");
    }

    /// One frame, as a terminal of that size would receive it.
    ///
    /// The one thing `*_lines` cannot answer: those functions say what the
    /// screen *says*, and a popup is about what it *covers*.
    fn frame_of(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, app, Theme::plain(), true))
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

    #[test]
    fn the_chooser_covers_the_transcript_rather_than_showing_through_it() {
        // Ratatui writes differences, so a cell a widget does not set is a cell
        // nobody wrote — which is the same reason the window has to clear the
        // alternate screen it took, one box smaller.
        let mut app = played(Kind::Claude, testing::CLAUDE);
        let reading = frame_of(&app, 80, 24);
        assert!(
            reading.iter().any(|row| row.contains("All tests pass.")),
            "{reading:#?}"
        );

        app.open_picker();
        let choosing = frame_of(&app, 80, 24);
        assert!(
            choosing.iter().any(|row| row.contains("new session")),
            "{choosing:#?}"
        );
        // Nothing from the transcript inside the box. Every row of it holds an
        // account name and nothing else, which is what would not be true if the
        // cells the list does not fill were left as they were found.
        let boxed: Vec<&String> = choosing.iter().filter(|row| row.contains('│')).collect();
        assert!(boxed.len() > 5, "{choosing:#?}");
        for row in &boxed {
            let inside = row.split('│').nth(1).unwrap_or_default();
            assert!(
                ["claude-", "codex-", "grok-"]
                    .iter()
                    .any(|name| inside.contains(name)),
                "{inside:?} showed through the box\n{choosing:#?}"
            );
        }
        // The footer is still the window's, and says what the arrows do now.
        assert!(
            choosing.last().is_some_and(|row| row.contains("account")),
            "{choosing:#?}"
        );
    }

    #[test]
    fn a_window_too_small_for_the_chooser_still_draws() {
        // A split terminal on a laptop. Every dimension here is arithmetic on
        // an area that can be smaller than the box it is centring.
        let mut app = played(Kind::Claude, testing::CLAUDE);
        app.open_picker();
        for (width, height) in [(80, 24), (30, 8), (12, 6), (4, 4)] {
            let _ = frame_of(&app, width, height);
        }
    }

    #[test]
    fn every_sign_in_riabuild_keeps_is_in_the_chooser() {
        // The bug: a window that opened three panes on account 1 and gave the
        // other twenty-four no way in at all, on a machine where a developer had
        // deliberately signed in to each of them.
        let app = App::new(every_account());
        let rows: Vec<String> = picker_lines(&app, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert_eq!(rows.len(), 27);
        for name in ["claude-1", "claude-9", "codex-1", "grok-3", "grok-9"] {
            assert!(
                rows.iter().any(|row| row.contains(name)),
                "{name} {rows:#?}"
            );
        }
    }

    #[test]
    fn a_long_title_never_pushes_the_sign_in_off_the_row() {
        // What the column is for. The title is any length and the sign-in is
        // what identifies the session, so the title is the half that gives way.
        let mut app = App::new(every_account());
        let mut pane = TestPane::new("s1".into(), Kind::Claude, String::new());
        pane.account = 7;
        pane.title = "work out why the nightly job has started timing out".into();
        app.add(pane);
        let row = frame_of(&app, 80, 8).remove(1);
        let column: String = row.chars().take(LIST_WIDTH as usize).collect();
        assert!(column.contains("claude-7"), "{column:?}");
        assert!(column.contains('…'), "{column:?}");
    }

    #[test]
    fn a_row_says_which_sign_in_it_is_running_under() {
        // Two panes on the same harness are otherwise identical until one of
        // them has been asked something.
        let mut app = App::new(every_account());
        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        let mut second = TestPane::new("s2".into(), Kind::Claude, String::new());
        second.account = 4;
        app.add(second);
        let rows: Vec<String> = list_lines(&app, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert!(rows[0].contains("claude-1"), "{rows:#?}");
        assert!(rows[1].contains("claude-4"), "{rows:#?}");
    }

    #[test]
    fn a_machine_with_no_accounts_says_so_rather_than_offering_nothing() {
        let app = App::new(Accounts::default());
        let rows: String = picker_lines(&app, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert!(rows.contains("no accounts"), "{rows}");
    }

    #[test]
    fn a_session_can_always_be_written_to() {
        // There is no ended state any more: a session is a thread id and a
        // spool, and both outlive every process. Whatever happened to the last
        // turn, the next one resumes it.
        let mut app = App::new(Accounts::default());
        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        app.set_running("s1", true);
        assert!(text_of(&compose_line(&app, Theme::plain())).contains("enter"));
    }

    #[test]
    fn an_empty_screen_tells_the_developer_what_to_press() {
        let app = App::new(Accounts::default());
        let list: String = list_lines(&app, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert!(list.contains("press n"), "{list}");
        let body: String = transcript_lines(None, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert!(body.contains("Press n"), "{body}");
    }
}
