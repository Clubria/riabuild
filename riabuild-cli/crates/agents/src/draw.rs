//! What the screen says.
//!
//! Every function here turns [`App`] into styled [`Line`]s and touches no
//! widget, no area and no terminal, so what is on screen is assertable without a
//! backend. Where those lines *go* is `frame.rs`. A renderer written as one
//! function would only be testable by drawing it into a buffer and reading
//! pixels back.
//!
//! Every colour comes from a [`Role`] through [`Theme::style`], never from a
//! literal — the same rule the rest of riabuild follows, and it matters more
//! here rather than less: ratatui will happily send a 24-bit escape to a
//! sixteen-colour terminal, so `Theme::style` is what stands between the palette
//! and an SSH session that cannot render it.

use ratatui::text::{Line, Span};
use riabuild_theme::{Role, Style, Theme};

use crate::account::Account;
use crate::app::{App, Entry, Focus, Pane, Row, State};

/// What every line-builder needs and none of them should look up twice.
///
/// `Copy`, because it is threaded through the whole of `frame.rs` and a
/// reference would put a lifetime on every helper for no benefit.
#[derive(Debug, Clone, Copy)]
pub struct Chrome<'a> {
    pub theme: Theme,
    /// Whether this terminal can be trusted with the block glyphs.
    pub unicode: bool,
    /// The repository every session in this window belongs to, `owner/repo`.
    ///
    /// `None` on a checkout with no GitHub remote, which is rendered as nothing
    /// rather than as a guess.
    pub repo: Option<&'a str>,
}

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
/// line: this one turns in place inside a rail row, where a glyph that changes
/// width would make the column jitter on every tick.
const SPINNER: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

/// The widest sign-in there is: `claude-9` is eight, and one space after it.
const ACCOUNT_WIDTH: usize = 9;

/// How wide the rail is, given the whole window.
///
/// A third of the window, held between a floor that fits `claude-9` and a title
/// and a ceiling that leaves the transcript — which is prose — a readable
/// measure. Dynamic rather than a constant because the emails an offer row
/// carries are any length, and a fixed twenty-eight columns could never show
/// one.
pub fn rail_width(total: u16) -> u16 {
    (total / 3).clamp(22, 40).min(total.saturating_sub(8))
}

/// `text`, ending in an ellipsis where it did not fit.
///
/// The alternative is what ratatui does by itself, which is to stop drawing at
/// the edge: a title cut mid-word with no mark reads as the whole title, and the
/// two sessions it was meant to tell apart look identical.
pub fn clip(text: &str, width: usize) -> String {
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

/// A group heading in the rail.
fn heading(text: &str, theme: Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), theme.style(Role::Muted)))
}

/// The cursor mark, which is a bar down the left edge rather than a reversed
/// row: the rail is read down that edge, and a full-width highlight fights the
/// state colour the row exists to show.
fn cursor_mark(selected: bool, unicode: bool) -> &'static str {
    match (selected, unicode) {
        (true, true) => "▌",
        (true, false) => ">",
        (false, _) => " ",
    }
}

/// The rail: the sessions in this checkout, and the sign-ins a new one can be
/// started under.
///
/// The two are separate groups and marked differently on purpose. An offer is
/// not a session — it has no directory, no spool and nothing to count — and
/// listing the two together is what made a window that had been asked nothing
/// report "3 sessions" on its first frame.
pub fn rail_lines(app: &App, chrome: Chrome<'_>, width: u16) -> Vec<Line<'static>> {
    let theme = chrome.theme;
    let inner = width.saturating_sub(2) as usize;
    let mut lines = vec![heading("SESSIONS", theme)];

    if app.panes.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none yet",
            theme.style(Role::Muted),
        )));
    }
    for (index, pane) in app.panes.iter().enumerate() {
        let selected = app.cursor == index;
        let mark = if pane.state() == State::Busy && chrome.unicode {
            SPINNER[app.tick % SPINNER.len()]
        } else {
            pane.state().mark(chrome.unicode)
        };
        // The sign-in leads, and at a fixed width: it is what tells two sessions
        // on one harness apart before either has been asked anything, and a
        // title is any length, so a title placed first is what pushes it off.
        let title_width = inner.saturating_sub(2 + ACCOUNT_WIDTH);
        lines.push(Line::from(vec![
            Span::styled(
                cursor_mark(selected, chrome.unicode),
                theme.style(Role::Brand),
            ),
            Span::styled(format!("{mark} "), state_style(pane.state(), theme)),
            Span::styled(
                format!("{:<ACCOUNT_WIDTH$}", pane.account_name()),
                theme.style(Role::Muted),
            ),
            Span::styled(
                clip(&pane.label(), title_width),
                theme.style(if selected { Role::Strong } else { Role::Muted }),
            ),
        ]));
    }

    lines.push(Line::from(String::new()));
    lines.push(heading("NEW SESSION", theme));
    for (index, account) in app.offers.iter().enumerate() {
        let selected = app.cursor == app.panes.len() + index;
        let name = account.name();
        let mut spans = vec![
            Span::styled(
                cursor_mark(selected, chrome.unicode),
                theme.style(Role::Brand),
            ),
            // A plus and never a state mark. An offer has no state — nothing is
            // running and nothing has failed — and a green dot beside one would
            // be claiming an idle session that does not exist.
            Span::styled("+ ", theme.style(Role::Brand)),
            Span::styled(
                name.clone(),
                theme.style(if selected { Role::Strong } else { Role::Muted }),
            ),
        ];
        // Dropped rather than truncated where it does not fit. Half an email
        // address identifies nobody, and a rail that is narrow today is wide
        // again the moment the developer resizes the window.
        if let Some(email) = app.login_of(account.kind, account.number) {
            let room = inner.saturating_sub(2 + name.chars().count());
            if email.chars().count() + 3 <= room {
                spans.push(Span::styled(" · ", theme.style(Role::Muted)));
                spans.push(Span::styled(email.to_string(), theme.style(Role::Muted)));
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The selected session's transcript.
///
/// Empty for an offer: there is no conversation, and `frame.rs` puts the splash
/// there instead. Saying "waiting for the first reply" about a session that has
/// not been created would be describing something that is not happening.
pub fn transcript_lines(pane: Option<&Pane>, theme: Theme, unicode: bool) -> Vec<Line<'static>> {
    let Some(pane) = pane else {
        return Vec::new();
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

/// What a pane says when the cursor is on an offer rather than on a session.
///
/// A sentence rather than a status, because nothing is happening yet: the only
/// thing worth saying is what typing would start, and under whose sign-in. Only
/// the vendor's name is accented — the rest is prose.
pub fn splash_lines(account: &Account, email: Option<&str>, theme: Theme) -> Vec<Line<'static>> {
    let mut login = vec![
        Span::styled("login: ", theme.style(Role::Muted)),
        Span::styled(account.name(), theme.style(Role::Strong)),
    ];
    if let Some(email) = email {
        login.push(Span::styled(" · ", theme.style(Role::Muted)));
        login.push(Span::styled(email.to_string(), theme.style(Role::Muted)));
    }
    vec![
        Line::from(vec![
            Span::styled("create a ", Style::default()),
            Span::styled(account.kind.short(), theme.style(Role::Brand)),
            Span::styled(" session", Style::default()),
        ]),
        Line::from(login),
    ]
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
            let tail = match app.login_of(account.kind, account.number) {
                Some(email) => email.to_string(),
                None => account.kind.label().to_string(),
            };
            Line::from(vec![
                Span::styled(cursor_mark(selected, unicode), theme.style(Role::Brand)),
                Span::styled(
                    format!("{:<10}", account.name()),
                    theme.style(if selected { Role::Strong } else { Role::Muted }),
                ),
                Span::styled(tail, theme.style(Role::Muted)),
            ])
        })
        .collect()
}

/// The window's own name, and the repository it is scoped to.
pub fn header_line(chrome: Chrome<'_>) -> Line<'static> {
    let theme = chrome.theme;
    let mut spans = vec![Span::styled("riabuild agents", theme.style(Role::Brand))];
    // Stated rather than implied. Every session in this window belongs to this
    // checkout — the store filters by it — and a window that did not say so
    // looked like every agent on the machine.
    if let Some(repo) = chrome.repo {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(repo.to_string(), theme.style(Role::Strong)));
    }
    Line::from(spans)
}

/// How many sessions there are, and how many are working.
///
/// Sessions only. An offer is not one, and counting the three the rail opens
/// with is the "3 sessions" bug written down.
pub fn counts_line(app: &App, theme: Theme) -> Line<'static> {
    let sessions = app.panes.len();
    let mut spans = vec![Span::styled(
        format!("{sessions} session{}", plural(sessions)),
        theme.style(Role::Muted),
    )];
    let busy = app.busy_count();
    if busy > 0 {
        spans.push(Span::styled(" · ", theme.style(Role::Muted)));
        spans.push(Span::styled(
            format!("{busy} working"),
            theme.style(Role::Busy),
        ));
    }
    Line::from(spans)
}

/// The pane's own first line: whose sign-in this is, and what it has spent.
///
/// Padded to `width` rather than rendered as two paragraphs, because the second
/// would repaint the cells of the first — including the raised background this
/// pane is drawn on.
pub fn status_line(app: &App, theme: Theme, width: u16) -> Line<'static> {
    let Some((name, kind, number)) = whose(app) else {
        return Line::from(String::new());
    };
    let mut left = vec![Span::styled(name.clone(), theme.style(Role::Strong))];
    let mut used = name.chars().count();
    if let Some(email) = app.login_of(kind, number) {
        left.push(Span::styled(" · ", theme.style(Role::Muted)));
        left.push(Span::styled(email.to_string(), theme.style(Role::Muted)));
        used += 3 + email.chars().count();
    }
    let spent = match app.selected() {
        Some(pane) if pane.input_tokens > 0 || pane.output_tokens > 0 => format!(
            "{} in / {} out",
            thousands(pane.input_tokens),
            thousands(pane.output_tokens)
        ),
        _ => String::new(),
    };
    if !spent.is_empty() {
        let gap = (width as usize).saturating_sub(used + spent.chars().count());
        if gap > 0 {
            left.push(Span::raw(" ".repeat(gap)));
            left.push(Span::styled(spent, theme.style(Role::Muted)));
        }
    }
    Line::from(left)
}

/// Which sign-in the pane is showing, session or offer.
fn whose(app: &App) -> Option<(String, riabuild_harness::Kind, usize)> {
    match app.row()? {
        Row::Session(index) => app
            .panes
            .get(index)
            .map(|pane| (pane.account_name(), pane.kind, pane.account)),
        Row::Offer(index) => app
            .offers
            .get(index)
            .map(|account| (account.name(), account.kind, account.number)),
    }
}

/// The key hints, which change with what the keyboard is talking to.
pub fn footer_line(app: &App, theme: Theme) -> Line<'static> {
    let keys: &[(&str, &str)] = match app.focus {
        Focus::List => &[
            ("↑↓", "move"),
            ("→", "open"),
            ("n", "sign-in"),
            ("q", "quit"),
        ],
        // No letters advertised: every one of them is a character in the box.
        Focus::Session => &[
            ("type", "to write"),
            ("enter", "send"),
            ("↑↓", "scroll"),
            ("←", "sessions"),
        ],
        Focus::Picker => &[("↑↓", "account"), ("enter", "choose"), ("esc", "back")],
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

/// The prompt box, which lives inside the pane and never across the window.
pub fn compose_line(app: &App, theme: Theme) -> Line<'static> {
    let mut spans = vec![Span::styled("› ", theme.style(Role::Brand))];
    let (before, after) = app.compose.split();
    if app.focus == Focus::Session {
        spans.push(Span::raw(before.to_string()));
        // A block rather than the terminal's own cursor, which would have to be
        // positioned and would blink wherever the last cell was written.
        spans.push(Span::styled("▏", theme.style(Role::Brand)));
        spans.push(Span::raw(after.to_string()));
    } else if app.compose.is_empty() {
        spans.push(Span::styled(
            "press → to write".to_string(),
            theme.style(Role::Muted),
        ));
    } else {
        // A half-written prompt stays on screen from the rail. Hiding it would
        // read as having lost it.
        spans.push(Span::styled(
            app.compose.text().to_string(),
            theme.style(Role::Muted),
        ));
    }
    Line::from(spans)
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Groups digits, because a token count is read as a magnitude.
pub fn thousands(value: u64) -> String {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::account::{Account, Accounts};
    use crate::app::Pane as TestPane;
    use riabuild_harness::{Kind, testing};
    use riabuild_theme::Depth;

    /// Every sign-in riabuild keeps, which is what the window is handed.
    pub(crate) fn every_account() -> Accounts {
        let mut all = Vec::new();
        for kind in Kind::ALL {
            for number in 1..=9 {
                all.push(Account::new(kind, number, None));
            }
        }
        Accounts::from(all)
    }

    pub(crate) fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn plain_chrome() -> Chrome<'static> {
        Chrome {
            theme: Theme::plain(),
            unicode: true,
            repo: Some("Clubria/riabuild"),
        }
    }

    fn played(kind: Kind, transcript: &str) -> App {
        let mut app = App::new(every_account());
        app.add(TestPane::new("s1".into(), kind, "the first prompt".into()));
        app.cursor = 0;
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
        let mut app = played(Kind::Claude, testing::CLAUDE);
        app.set_login(Kind::Claude, 1, "ada@clubria.com".into());
        let sixteen = Theme::with_depth(Depth::Ansi16);
        let chrome = Chrome {
            theme: sixteen,
            unicode: true,
            repo: Some("Clubria/riabuild"),
        };
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
        lines.extend(rail_lines(&app, chrome, 30));
        lines.push(header_line(chrome));
        lines.push(counts_line(&app, sixteen));
        lines.push(status_line(&app, sixteen, 60));
        lines.push(footer_line(&app, sixteen));
        lines.push(compose_line(&app, sixteen));
        lines.extend(picker_lines(&app, sixteen, true));
        lines.extend(splash_lines(
            &Account::new(Kind::Claude, 1, None),
            Some("ada@clubria.com"),
            sixteen,
        ));

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
        let chrome = Chrome {
            theme: plain,
            unicode: false,
            repo: None,
        };
        let rail: String = rail_lines(&app, chrome, 30).iter().map(text_of).collect();
        assert!(rail.contains('!'), "{rail}");
        assert!(!rail.contains('▲'), "{rail}");
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
    fn an_offer_is_never_counted_as_a_session() {
        // The complaint this redesign started from: a window that had been
        // asked nothing said "3 sessions", because opening a pane per harness
        // was how the three sign-ins were offered.
        let app = App::new(every_account());
        let counts = text_of(&counts_line(&app, Theme::plain()));
        assert!(counts.contains("0 sessions"), "{counts}");
        assert!(!counts.contains("working"), "{counts}");

        let mut app = app;
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        let one = text_of(&counts_line(&app, Theme::plain()));
        // Singular, because "1 sessions" is the kind of detail that makes a
        // tool feel unfinished.
        assert!(one.contains("1 session"), "{one}");
        app.sent("do the thing");
        let busy = text_of(&counts_line(&app, Theme::plain()));
        assert!(busy.contains("1 working"), "{busy}");
    }

    #[test]
    fn the_rail_separates_what_is_running_from_what_could_be_started() {
        let app = App::new(every_account());
        let rows: Vec<String> = rail_lines(&app, plain_chrome(), 30)
            .iter()
            .map(text_of)
            .collect();
        assert_eq!(rows[0], "SESSIONS");
        assert!(rows[1].contains("none yet"), "{rows:#?}");
        assert!(rows.iter().any(|row| row == "NEW SESSION"), "{rows:#?}");
        // An offer carries a `+` and never a state mark: nothing is running, so
        // a green dot beside one would be claiming an idle session.
        let offers: Vec<&String> = rows.iter().filter(|row| row.contains("+ ")).collect();
        assert_eq!(offers.len(), 3, "{rows:#?}");
        for (row, kind) in offers.iter().zip(Kind::ALL) {
            assert!(row.contains(kind.tag()), "{row}");
        }
    }

    #[test]
    fn a_sign_in_carries_the_email_it_belongs_to() {
        // What a developer with nine Claude accounts actually needs: `claude-1`
        // says which login it is only to riabuild.
        let mut app = App::new(every_account());
        app.set_login(Kind::Claude, 1, "ada@clubria.com".into());
        let wide: String = rail_lines(&app, plain_chrome(), 40)
            .iter()
            .map(text_of)
            .collect();
        assert!(wide.contains("claude-1 · ada@clubria.com"), "{wide}");
        // Dropped rather than cut in half where the rail is narrow: half an
        // address identifies nobody.
        let narrow: String = rail_lines(&app, plain_chrome(), 22)
            .iter()
            .map(text_of)
            .collect();
        assert!(narrow.contains("claude-1"), "{narrow}");
        assert!(!narrow.contains("ada@"), "{narrow}");
    }

    #[test]
    fn a_session_says_which_sign_in_and_which_login_it_is_running_under() {
        let mut app = App::new(every_account());
        app.set_login(Kind::Claude, 2, "ada@clubria.com".into());
        app.begin("s1".into(), &Account::new(Kind::Claude, 2, None));
        let status = text_of(&status_line(&app, Theme::plain(), 60));
        assert!(status.starts_with("claude-2 · ada@clubria.com"), "{status}");
    }

    #[test]
    fn a_sign_in_nobody_has_asked_about_yet_claims_nothing() {
        // The probe is a subprocess per account and answers late. An unknown
        // login renders as nothing rather than as "signed out", which would be
        // a claim riabuild has not established.
        let mut app = App::new(every_account());
        app.begin("s1".into(), &Account::new(Kind::Claude, 2, None));
        let status = text_of(&status_line(&app, Theme::plain(), 60));
        assert_eq!(status.trim(), "claude-2");
    }

    #[test]
    fn an_offer_says_what_typing_would_start_rather_than_waiting_for_a_reply() {
        // "waiting for the first reply…" was said over a pane that had no
        // session behind it at all, so there was nothing to wait for.
        let account = Account::new(Kind::Claude, 1, None);
        let lines: Vec<String> = splash_lines(&account, Some("ada@clubria.com"), Theme::plain())
            .iter()
            .map(text_of)
            .collect();
        assert_eq!(
            lines,
            vec![
                "create a Claude session".to_string(),
                "login: claude-1 · ada@clubria.com".to_string(),
            ]
        );
        // and only the vendor's name is accented — the rest is prose
        let brand = Theme::with_depth(Depth::TrueColor);
        let first = splash_lines(&account, None, brand).remove(0);
        assert_eq!(first.spans[1].content.as_ref(), "Claude");
        assert_eq!(first.spans[1].style, brand.style(Role::Brand));
        assert_eq!(first.spans[0].style, Style::default());
    }

    #[test]
    fn the_window_says_which_repository_it_is_scoped_to() {
        let header = text_of(&header_line(plain_chrome()));
        assert!(header.contains("Clubria/riabuild"), "{header}");
        // A checkout with no remote says nothing rather than guessing.
        let bare = text_of(&header_line(Chrome {
            theme: Theme::plain(),
            unicode: true,
            repo: None,
        }));
        assert_eq!(bare, "riabuild agents");
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
        let mut app = App::new(every_account());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        app.cursor = 0;
        app.focus = Focus::List;
        let listing = text_of(&footer_line(&app, Theme::plain()));
        assert!(listing.contains("move"), "{listing}");
        assert!(listing.contains("quit"), "{listing}");

        app.focus = Focus::Session;
        let writing = text_of(&footer_line(&app, Theme::plain()));
        assert!(writing.contains("send"), "{writing}");
        assert!(writing.contains("scroll"), "{writing}");
        // No letter is advertised while typing: each one is a character then.
        assert!(!writing.contains("quit"), "{writing}");
    }

    #[test]
    fn a_half_written_prompt_survives_a_trip_to_the_rail() {
        let mut app = App::new(every_account());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        app.focus = Focus::Session;
        for ch in "hello".chars() {
            app.compose.insert(ch);
        }
        assert!(text_of(&compose_line(&app, Theme::plain())).contains("hello"));
        app.focus = Focus::List;
        let from_rail = text_of(&compose_line(&app, Theme::plain()));
        assert!(from_rail.contains("hello"), "{from_rail}");
        // and an empty box says how to reach it rather than nothing at all
        app.compose.take();
        assert!(text_of(&compose_line(&app, Theme::plain())).contains("press →"));
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
    fn a_machine_with_no_accounts_says_so_rather_than_offering_nothing() {
        let app = App::new(Accounts::default());
        let rows: String = picker_lines(&app, Theme::plain(), true)
            .iter()
            .map(text_of)
            .collect();
        assert!(rows.contains("no accounts"), "{rows}");
    }
}
