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
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
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
                Span::styled(
                    pane.label(),
                    theme.style(if selected { Role::Strong } else { Role::Muted }),
                ),
                Span::styled(format!(" {}", pane.kind.tag()), theme.style(Role::Muted)),
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
        Focus::List => &[
            ("↑↓", "session"),
            ("enter", "write"),
            ("n", "new"),
            ("pgup/pgdn", "scroll"),
            ("q", "quit"),
        ],
        Focus::Compose => &[("enter", "send"), ("esc", "back")],
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

    // A fixed 28 columns for the list: wide enough for a repository name and
    // narrow enough that the transcript, which is prose, keeps a readable
    // measure on an 80-column terminal.
    let body = Layout::horizontal([Constraint::Length(28), Constraint::Min(20)]).split(rows[1]);
    render_list(frame, app, theme, unicode, body[0]);
    render_transcript(frame, app, theme, unicode, body[1]);

    frame.render_widget(Paragraph::new(compose_line(app, theme)), rows[2]);
    frame.render_widget(Paragraph::new(footer_line(app, theme)), rows[3]);
}

fn render_list(frame: &mut Frame, app: &App, theme: Theme, unicode: bool, area: Rect) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(theme.style(Role::Muted));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane as TestPane;
    use riabuild_harness::{Kind, testing};
    use riabuild_theme::Depth;

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn played(kind: Kind, transcript: &str) -> App {
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
        let mut app = App::new();
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
    fn token_counts_are_grouped_so_a_magnitude_is_readable() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(28_431), "28,431");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn the_footer_says_what_the_keyboard_is_talking_to() {
        let mut app = App::new();
        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        assert!(text_of(&footer_line(&app, Theme::plain())).contains("new"));
        app.focus = Focus::Compose;
        let writing = text_of(&footer_line(&app, Theme::plain()));
        assert!(writing.contains("send"), "{writing}");
        // `n` must not be advertised while typing: it is a letter then.
        assert!(!writing.contains("new"), "{writing}");
    }

    #[test]
    fn a_session_can_always_be_written_to() {
        // There is no ended state any more: a session is a thread id and a
        // spool, and both outlive every process. Whatever happened to the last
        // turn, the next one resumes it.
        let mut app = App::new();
        app.add(TestPane::new("s1".into(), Kind::Claude, String::new()));
        app.set_running("s1", true);
        assert!(text_of(&compose_line(&app, Theme::plain())).contains("enter"));
    }

    #[test]
    fn an_empty_screen_tells_the_developer_what_to_press() {
        let app = App::new();
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
