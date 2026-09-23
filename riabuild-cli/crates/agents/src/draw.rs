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
use crate::activity::Activity;
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

/// What the rail calls a delegated session.
const SUBAGENT: &str = "(subagent)";

/// The mark that puts a delegated session under the one that asked for it.
///
/// The same glyph `transcript_lines` indents a subagent's *work* with, so the
/// rail and the transcript make the same claim the same way. `store::arrange`
/// is what makes the row above it the right one.
fn child_mark(unicode: bool) -> &'static str {
    if unicode { "↳ " } else { "> " }
}

/// `(subagent)` with the space before it, or nothing where the rail is too
/// narrow to carry it and a title.
///
/// The floor is a title, not the tag: a row reading `↳ codex-1  (subagent)` with
/// the prompt clipped away says which kind of thing it is and nothing about
/// which one, and the indent already said the first part.
/// It rides on a row's **second** line rather than its first, which is what the
/// second line bought: the tag used to eat the tail of the one line a title had,
/// so the rows that most needed identifying were the ones with least room to do
/// it. The title now gets the whole of the first line either way.
fn tag_for(inner: usize, indent: usize) -> Option<String> {
    const TITLE_FLOOR: usize = 8;
    let room = inner.saturating_sub(2 + ACCOUNT_WIDTH + indent);
    (room >= SUBAGENT.chars().count() + 1 + TITLE_FLOOR).then(|| format!(" {SUBAGENT}"))
}

/// How many lines of the rail one session takes.
///
/// Two, always — the second one blank where the title fits on the first. A row
/// whose height depended on its title would make the rail reflow every time an
/// agent was asked something new, and the blank line is what gives a list of
/// sessions any air at all.
pub const ROW_LINES: usize = 2;

/// A title laid across two lines of `first` and `second` columns.
///
/// Broken at a space where there is one inside the first line, so a title reads
/// as prose rather than as a string cut at a column. The tail is clipped with an
/// ellipsis by [`clip`], which is the mark that says a title goes on — without
/// it two different sessions truncate to the same row and look like one.
fn across_two(text: &str, first: usize, second: usize) -> (String, String) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= first {
        return (text.to_string(), String::new());
    }
    // The last space at or before the break, so the first line ends on a word.
    // A first word longer than the line — a path, a stack frame — has none, and
    // is cut at the edge rather than pushed whole onto the second line, which
    // would leave the first blank.
    let cut = chars[..=first.min(chars.len() - 1)]
        .iter()
        .rposition(|ch| *ch == ' ')
        .unwrap_or(first);
    let head: String = chars[..cut].iter().collect();
    let tail: String = chars[cut..].iter().collect();
    (head.trim_end().to_string(), clip(tail.trim_start(), second))
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
        let child = pane.parent.is_some();
        let indent = if child {
            child_mark(chrome.unicode)
        } else {
            ""
        };
        // Characters and never bytes: `↳` is one column and three bytes, and a
        // width computed from `len()` would take a column off the title on
        // every child row.
        let indent_width = indent.chars().count();
        // The tag is dropped rather than truncated where the rail is narrow,
        // for the reason the offer row below drops an email: `rail_width` goes
        // down to twenty-two columns, and half of "(subagent)" beside a title
        // clipped to nothing identifies neither. The indent survives as the
        // signal, which is what it is there for.
        let tag = child.then(|| tag_for(inner, indent_width)).flatten();
        // The title column, and it is the same on both of a row's lines: the
        // second is a continuation, so a developer reads one paragraph down one
        // column rather than a title that jumps left when it wraps.
        let title_width = inner.saturating_sub(2 + ACCOUNT_WIDTH + indent_width);
        let tag_width = tag.as_ref().map(|tag| tag.chars().count()).unwrap_or(0);
        let (head, tail) = across_two(
            &pane.label(),
            title_width,
            title_width.saturating_sub(tag_width),
        );
        let title_style = theme.style(if selected { Role::Strong } else { Role::Muted });
        lines.push(Line::from(vec![
            Span::styled(
                cursor_mark(selected, chrome.unicode),
                theme.style(Role::Brand),
            ),
            // Dimmed for a child that is *idle*, and only then. A subagent
            // sitting quietly beside its parent is the row this whole treatment
            // exists to push into the background — but one that is working or
            // has failed keeps the hue every other row uses for that, because a
            // failed subagent a developer cannot see is the same bug as a failed
            // session they cannot see.
            Span::styled(
                format!("{mark} "),
                if child && pane.state() == State::Idle {
                    theme.style(Role::Muted)
                } else {
                    state_style(pane.state(), theme)
                },
            ),
            Span::styled(indent.to_string(), theme.style(Role::Muted)),
            Span::styled(
                format!("{:<ACCOUNT_WIDTH$}", pane.account_name()),
                theme.style(Role::Muted),
            ),
            Span::styled(head, title_style),
        ]));

        // The second line: the rest of the title under the first, and the
        // cursor's bar carried down it so a selected row reads as one block
        // rather than as a marked line with an unmarked one under it.
        let mut second = vec![
            Span::styled(
                cursor_mark(selected, chrome.unicode),
                theme.style(Role::Brand),
            ),
            Span::raw(" ".repeat(2 + indent_width + ACCOUNT_WIDTH)),
            Span::styled(tail.clone(), title_style),
        ];
        if let Some(tag) = tag {
            // Padded out to the right edge here rather than by a second
            // `Line`-level alignment, because a `Line` carries one alignment for
            // all of its spans and this row is left-aligned everywhere else.
            let gap = title_width
                .saturating_sub(tag_width)
                .saturating_sub(tail.chars().count());
            second.push(Span::styled(
                format!("{}{tag}", " ".repeat(gap)),
                theme.style(Role::Muted),
            ));
        }
        lines.push(Line::from(second));
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
    if app.offers.is_empty() {
        if app.checking {
            lines.push(Line::from(Span::styled(
                "  checking sign-ins…",
                theme.style(Role::Muted),
            )));
        } else {
            // One sentence, wrapped to the rail rather than cut: it is the only
            // thing on the rail that says why there is nothing to start, and the
            // commands at the end of it are the part worth reading.
            for row in wrap_words(&nothing_signed_in(), inner.saturating_sub(2).max(1)) {
                lines.push(Line::from(Span::styled(
                    format!("  {row}"),
                    theme.style(Role::Muted),
                )));
            }
        }
    }
    lines
}

/// What NEW SESSION says when nothing is signed in.
///
/// Signing in is the developer's to do, outside this window, with each
/// harness's own command — so the window names the commands and does nothing
/// else. It is asked once, when the window opens, which is why the sentence
/// ends by saying to open it again.
pub fn nothing_signed_in() -> String {
    let commands: Vec<String> = riabuild_harness::Kind::ALL
        .into_iter()
        .map(|kind| format!("`{}`", crate::account::sign_in_command(kind)))
        .collect();
    format!(
        "Nothing is signed in. Sign in outside riabuild agents with {}, then open it again.",
        commands.join(", ")
    )
}

/// Breaks `text` into rows of at most `width` characters, at spaces.
///
/// A command in backticks is one word, never broken inside: split across two
/// rows it is one nobody can copy. A word longer than the row is left whole on
/// a row of its own, where the frame cuts it.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut quoted = false;
    for word in text.split(' ') {
        match words.last_mut() {
            Some(last) if quoted => {
                last.push(' ');
                last.push_str(word);
            }
            _ => words.push(word.to_string()),
        }
        if word.matches('`').count() % 2 == 1 {
            quoted = !quoted;
        }
    }
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in words.iter().filter(|word| !word.is_empty()) {
        if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > width {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Which line of [`rail_lines`] the cursor is on.
///
/// Arithmetic rather than a second pass over the rows, and it has to exist
/// because a session is two lines now: ten sessions no longer fit in a body that
/// held them when each was one, so `frame.rs` scrolls the rail to keep the
/// cursor in view. Every constant here is the shape `rail_lines` builds, and the
/// two are pinned together by a test rather than by a comment.
pub fn rail_cursor_line(app: &App) -> usize {
    // The `SESSIONS` heading.
    let sessions_at = 1;
    // An empty list still draws a line saying so.
    let sessions_height = if app.panes.is_empty() {
        1
    } else {
        app.panes.len() * ROW_LINES
    };
    match app.row() {
        Some(Row::Session(index)) => sessions_at + index * ROW_LINES,
        // …the blank row, then the `NEW SESSION` heading.
        Some(Row::Offer(index)) => sessions_at + sessions_height + 2 + index,
        None => 0,
    }
}

/// The selected session's transcript.
///
/// Empty for an offer: there is no conversation, and `frame.rs` puts the splash
/// there instead. Empty for a session with nothing in it too — what a turn is
/// doing is [`activity_line`]'s to say, and a placeholder here would be a second
/// voice saying it, wrongly as soon as the turn had failed.
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
    lines
}

/// The one line saying what the selected session's turn is doing, or `None`
/// when no turn is running — which is how "stopped" is shown: by not being
/// shown at all.
///
/// Three shapes, one spinner:
///
/// - `⠹ launching claude session…` — started, and the harness has said nothing.
/// - `⠹ thinking… · claude opus-5 at high` — writing, no tool call open. The
///   model and the effort are whatever the harness reported, and each is left
///   out where it reported nothing rather than filled in with a guess.
/// - `⠹ running Bash  pnpm test login` — a tool call is out, in the working
///   colour so it reads as the agent waiting on something rather than thinking.
///
/// Cut to `width` rather than wrapped: it is a status, and a status that grew a
/// second row would push the conversation up and down as tools came and went.
pub fn activity_line(
    pane: Option<&Pane>,
    chrome: Chrome<'_>,
    tick: usize,
    width: u16,
) -> Option<Line<'static>> {
    let pane = pane?;
    let activity = pane.activity()?;
    let theme = chrome.theme;
    let ellipsis = if chrome.unicode { "…" } else { "..." };
    let spinner = if chrome.unicode {
        SPINNER[tick % SPINNER.len()]
    } else {
        "~"
    };
    let tag = pane.kind.tag();
    let mut parts: Vec<(String, Style)> = vec![(format!("{spinner} "), theme.style(Role::Busy))];
    match activity {
        Activity::Launching => parts.push((
            format!("launching {tag} session{ellipsis}"),
            theme.style(Role::Muted),
        )),
        Activity::Thinking => {
            parts.push((format!("thinking{ellipsis}"), Style::default()));
            let mut about = format!(" · {tag}");
            if let Some(model) = pane
                .model
                .as_deref()
                .map(|model| model_name(pane.kind, model))
                && !model.is_empty()
            {
                about.push(' ');
                about.push_str(&model);
            }
            if let Some(effort) = pane.effort.as_deref().filter(|effort| !effort.is_empty()) {
                about.push_str(" at ");
                about.push_str(effort);
            }
            parts.push((about, theme.style(Role::Muted)));
        }
        Activity::Tool { name, detail } => {
            parts.push(("running ".to_string(), theme.style(Role::Busy)));
            parts.push((name.to_string(), theme.style(Role::Strong)));
            if let Some(detail) = detail {
                parts.push((format!("  {detail}"), theme.style(Role::Muted)));
            }
        }
    }

    // Cut from the right, a span at a time, so the spinner and the verb are
    // always there and only the detail loses its end.
    let mut room = width as usize;
    let mut spans = Vec::new();
    for (text, style) in parts {
        if room == 0 {
            break;
        }
        let fitted = clip(&text, room);
        room = room.saturating_sub(fitted.chars().count());
        spans.push(Span::styled(fitted, style));
    }
    Some(Line::from(spans))
}

/// A model id as the indicator shows it.
///
/// Claude Code reports `claude-opus-5[1m]`: the harness is already named beside
/// it, so the vendor prefix is a stutter, and the bracket is a context-window
/// variant rather than a different model. Everything else is shown as reported.
fn model_name(kind: riabuild_harness::Kind, model: &str) -> String {
    let bare = match model.find('[') {
        Some(at) => &model[..at],
        None => model,
    };
    let bare = match kind {
        riabuild_harness::Kind::Claude => bare.strip_prefix("claude-").unwrap_or(bare),
        _ => bare,
    };
    bare.to_string()
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

/// What an offer's pane says when the last attempt to start a session under it
/// failed, in place of the splash.
///
/// Every line of the error kept, and all of it in the danger colour: this is
/// the only report of the failure there is, because no session exists for it
/// to go in. `frame.rs` centres it both ways, the way it centres the splash.
pub fn failure_lines(why: &str, theme: Theme) -> Vec<Line<'static>> {
    why.lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme.style(Role::Danger))))
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
    // The session's total over every turn, not the last turn's count.
    let spent = match app.selected().map(Pane::tokens) {
        Some((input, output)) if input > 0 || output > 0 => {
            format!("{} in / {} out", thousands(input), thousands(output))
        }
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
///
/// A hint that does not fit is **dropped whole** rather than cut at the edge.
/// The list grew when the box learned to break a line, and on a split terminal
/// ratatui simply stopped drawing partway through the last one — which renders
/// as `← se`, a key hint that is not a key and not a word. Dropping from the
/// right keeps the ones a developer needs most, which is why the order they are
/// written in is the order they matter in.
pub fn footer_line(app: &App, theme: Theme, width: u16) -> Line<'static> {
    // A notice takes the whole line while it lasts. It is the answer to the key
    // just pressed, and the hints it stands in front of are still true — showing
    // both would make the one thing worth reading the shorter half of the line.
    if let Some(notice) = &app.notice {
        return Line::from(Span::styled(notice.clone(), theme.style(Role::Warn)));
    }
    let keys: &[(&str, &str)] = match app.focus {
        Focus::List => &[("↑↓", "move"), ("→", "open"), ("q", "quit")],
        // No letters advertised: every one of them is a character in the box.
        Focus::Session => &[
            ("enter", "send"),
            // Advertised because it is the one key here a developer would
            // otherwise assume their terminal had eaten: Ctrl-V reaching this
            // window at all is unusual, and an image is the thing they cannot
            // type.
            ("^v", "paste"),
            // The line break, which is the gesture nobody guesses: Enter sends,
            // so a developer who wants two paragraphs has no way to find this
            // by trying things.
            ("alt+enter", "newline"),
            ("↑↓", "scroll"),
            ("←", "sessions"),
        ],
    };
    let mut spans = Vec::new();
    let mut used = 0usize;
    for (index, (key, what)) in keys.iter().enumerate() {
        let separator = if index > 0 { 3 } else { 0 };
        let wants = separator + key.chars().count() + 1 + what.chars().count();
        if used + wants > width as usize {
            break;
        }
        used += wants;
        if index > 0 {
            spans.push(Span::styled(" · ", theme.style(Role::Muted)));
        }
        spans.push(Span::styled((*key).to_string(), theme.style(Role::Strong)));
        spans.push(Span::styled(format!(" {what}"), theme.style(Role::Muted)));
    }
    Line::from(spans)
}

/// The mark the box opens with, and the width it costs every row.
///
/// The continuation rows are indented by the same amount rather than starting at
/// the pane's edge, so the prompt is one block of text with one left margin
/// instead of a first line that is inset and a second that is not.
const COMPOSE_MARK: &str = "› ";
pub const COMPOSE_INDENT: usize = 2;

/// How many columns of a box `width` wide the text may wrap into, once the
/// caret's own cell is set aside.
///
/// The caret is drawn as an extra cell beside the character it sits before or
/// after, never by styling a cell already spoken for — see [`compose_lines`].
/// A row wrapped all the way to the edge of the box would then need one column
/// more than the box has the moment the caret landed on it, which is exactly
/// the column with nowhere to go: ratatui does not wrap a `Line` that runs past
/// its `Rect`, it stops drawing at the edge, so that one cell — the caret,
/// mid-prompt, on whatever keystroke first filled the row — silently never
/// appeared. Reserving it here, in the same width both [`compose_lines`] and
/// `frame::render_compose` wrap by, is what keeps every row inside the box
/// regardless of where the caret is on it.
pub fn compose_wrap_width(width: u16) -> usize {
    (width as usize)
        .saturating_sub(COMPOSE_INDENT)
        .saturating_sub(1)
        .max(1)
}

/// The prompt box, which lives inside the pane and never across the window.
///
/// Many lines rather than one. A prompt is prose and prose is longer than a
/// pane is wide, so a single line ran off the right edge and took the caret with
/// it — the half of a paragraph a developer had just written was somewhere they
/// could not see. `Compose::wrap` decides where the breaks fall, because the
/// caret has to land on the same row its character does and only the editor
/// knows both.
pub fn compose_lines(app: &App, theme: Theme, unicode: bool, width: u16) -> Vec<Line<'static>> {
    let room = (width as usize).saturating_sub(COMPOSE_INDENT);
    if app.focus != Focus::Session {
        let (text, role) = if app.compose.is_empty() {
            ("press → to write".to_string(), Role::Muted)
        } else {
            // A half-written prompt stays on screen from the rail. Hiding it
            // would read as having lost it.
            (app.compose.text().to_string(), Role::Muted)
        };
        return vec![Line::from(vec![
            Span::styled(COMPOSE_MARK, theme.style(Role::Brand)),
            Span::styled(clip(&text.replace('\n', " "), room), theme.style(role)),
        ])];
    }

    let wrapped = app.compose.wrap(compose_wrap_width(width));
    let (caret_row, caret_column) = wrapped.caret;
    // ASCII where the terminal cannot be trusted with the block glyph, the
    // same fallback every other mark in this file offers — see `cursor_mark`.
    // A caret with no fallback is the one glyph in the box that is always on
    // screen while a developer is typing, so a terminal that renders it as
    // nothing at all reads as a blank cell beside every character they type.
    let caret_glyph = if unicode { "▏" } else { "|" };
    wrapped
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            // The mark on the first row only: it opens the prompt rather than
            // labelling each of its lines.
            let lead = if index == 0 {
                Span::styled(COMPOSE_MARK, theme.style(Role::Brand))
            } else {
                Span::raw(" ".repeat(COMPOSE_INDENT))
            };
            // A newline is a break, not a glyph: it is what put this row's
            // successor on its own line and drawing it would paint a stray cell.
            let text: String = row.chars().filter(|ch| *ch != '\n').collect();
            if index != caret_row {
                return Line::from(vec![lead, Span::raw(text)]);
            }
            let at = text
                .char_indices()
                .nth(caret_column)
                .map(|(byte, _)| byte)
                .unwrap_or(text.len());
            let (before, after) = text.split_at(at);
            Line::from(vec![
                lead,
                Span::raw(before.to_string()),
                // A block rather than the terminal's own cursor, which would
                // have to be positioned and would blink wherever the last cell
                // was written.
                Span::styled(caret_glyph, theme.style(Role::Brand)),
                Span::raw(after.to_string()),
            ])
        })
        .collect()
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
    use crate::account::tests::first_of_each;
    use crate::account::{Account, SignedIn};
    use crate::app::Pane as TestPane;
    use riabuild_harness::{Kind, testing};
    use riabuild_theme::Depth;

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
        played_with(kind, transcript, first_of_each())
    }

    fn played_with(kind: Kind, transcript: &str, signed_in: Vec<SignedIn>) -> App {
        let mut app = App::offering(signed_in);
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

    /// Ctrl-V is advertised because it is the one key here a developer would
    /// otherwise assume their terminal had eaten, and a notice takes the line
    /// while it lasts — the hints are still true, but they are not the thing
    /// worth reading.
    #[test]
    fn the_footer_offers_paste_and_gives_the_line_up_for_a_notice() {
        let theme = Theme::with_depth(Depth::Ansi16);
        let mut app = App::new();
        app.focus = Focus::Session;
        let hints: String = footer_line(&app, theme, 120)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(hints.contains("^v paste"), "{hints}");

        app.notice = Some("Nothing on the clipboard to paste.".into());
        let notice: String = footer_line(&app, theme, 120)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(notice, "Nothing on the clipboard to paste.");
    }

    #[test]
    fn a_hint_that_does_not_fit_is_dropped_whole_rather_than_cut() {
        // A split terminal. Ratatui draws a too-long line by stopping partway
        // through it, which renders the last hint as `← se` — not a key and not
        // a word. The ones that matter most are written first, so what goes is
        // what is worth least.
        let mut app = App::new();
        app.focus = Focus::Session;
        let theme = Theme::plain();
        let whole = text_of(&footer_line(&app, theme, 120));
        assert!(whole.contains("← sessions"), "{whole}");

        let narrow = text_of(&footer_line(&app, theme, 30));
        assert!(narrow.chars().count() <= 30, "{narrow:?}");
        assert!(narrow.starts_with("enter send"), "{narrow:?}");
        // Whole hints only: nothing ends mid-word or on a separator.
        assert!(!narrow.ends_with(' '), "{narrow:?}");
        assert!(!narrow.contains("← se\u{0}"), "{narrow:?}");
        for hint in ["^v paste", "alt+enter newline", "↑↓ scroll", "← sessions"] {
            let present = narrow.contains(hint);
            let partial = hint
                .char_indices()
                .any(|(at, _)| at > 0 && narrow.ends_with(&hint[..at]));
            assert!(present || !partial, "{hint:?} was cut\n{narrow:?}");
        }
    }

    #[test]
    fn every_colour_on_screen_comes_from_the_palette() {
        // The rule the rest of riabuild follows, and the one ratatui makes easy
        // to break: a literal `Color::Rgb` here would reach a sixteen-colour
        // terminal as an escape it cannot read.
        let app = played_with(
            Kind::Claude,
            testing::CLAUDE,
            vec![SignedIn::new(
                Account::new(Kind::Claude, 1, None),
                Some("ada@clubria.com".into()),
            )],
        );
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
        lines.push(footer_line(&app, sixteen, 120));
        // The footer has two shapes and the notice is the one that only appears
        // after a key was pressed — exactly the sort of line a palette check
        // over one frame never reaches.
        let mut noticed = App::new();
        noticed.notice = Some("Nothing on the clipboard to paste.".into());
        lines.push(footer_line(&noticed, sixteen, 120));
        lines.extend(compose_lines(&app, sixteen, true, 40));
        lines.extend(splash_lines(
            &Account::new(Kind::Claude, 1, None),
            Some("ada@clubria.com"),
            sixteen,
        ));
        // The two shapes NEW SESSION takes when it has no rows: still asking,
        // and nothing signed in.
        lines.extend(rail_lines(&App::new(), chrome, 30));
        lines.extend(rail_lines(&App::offering(Vec::new()), chrome, 30));

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
    fn an_offer_is_never_counted_as_a_session() {
        // The complaint this redesign started from: a window that had been
        // asked nothing said "3 sessions", because opening a pane per harness
        // was how the three sign-ins were offered.
        let app = App::offering(first_of_each());
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

    /// A Claude session with a Codex subagent under it, which is the shape
    /// `store::arrange` hands the window.
    fn with_a_subagent() -> App {
        let mut app = App::offering(first_of_each());
        app.add(TestPane::new(
            "parent".into(),
            Kind::Claude,
            "port the parser".into(),
        ));
        // Short enough to survive the clip at forty columns, so a test looking
        // for it is testing the indent rather than the ellipsis.
        let mut child = TestPane::new("child".into(), Kind::Codex, "write tests".into());
        child.parent = Some("parent".into());
        app.add(child);
        app.cursor = 0;
        app
    }

    fn rows_of(app: &App, width: u16) -> Vec<String> {
        rail_lines(app, plain_chrome(), width)
            .iter()
            .map(text_of)
            .collect()
    }

    #[test]
    fn a_subagent_is_indented_and_tagged_and_its_parent_is_neither() {
        // Two lines per session, so the shape is: heading, the parent's pair,
        // then the child's. The indent is on the line with the title on it and
        // the tag is on the one under it — the title has the whole first line
        // either way, which is what the second line was for.
        let rows = rows_of(&with_a_subagent(), 40);
        let parent = &rows[1..3];
        let child = &rows[3..5];

        assert!(child[0].contains("write tests"), "{child:#?}");
        assert!(child[0].contains('↳'), "{child:#?}");
        assert!(child[1].contains("(subagent)"), "{child:#?}");
        assert!(parent[0].contains("port the parser"), "{parent:#?}");
        assert!(!parent.iter().any(|row| row.contains('↳')), "{parent:#?}");
        assert!(
            !parent.iter().any(|row| row.contains("(subagent)")),
            "{parent:#?}"
        );
        // Right-aligned: the tag ends the line, and the line fills the rail.
        assert!(child[1].ends_with("(subagent)"), "{child:#?}");
        assert_eq!(child[1].chars().count(), 39, "{child:#?}");
    }

    #[test]
    fn a_long_title_carries_on_under_itself_rather_than_being_cut_at_the_first_line() {
        // The whole of why a session is two lines. The rail is the only place a
        // developer tells two conversations apart, and one line of a forty-
        // column rail is eighteen characters of prompt.
        let mut app = App::offering(first_of_each());
        app.add(TestPane::new(
            "s1".into(),
            Kind::Claude,
            "fix the total count of connections in the pool report".into(),
        ));
        app.cursor = 0;
        let rows = rows_of(&app, 40);

        assert!(rows[1].contains("claude-1"), "{rows:#?}");
        assert!(rows[1].contains("fix the total count"), "{rows:#?}");
        // The break falls between words rather than mid-word, and the second
        // line carries on under the first — aligned to the same column, so it
        // reads as one paragraph rather than two rows.
        // Past the cursor's bar, which is carried down the second line too.
        assert!(
            rows[2]
                .trim_start_matches(['▌', ' '])
                .starts_with("connections"),
            "{rows:#?}"
        );
        // In *characters*: the mark on the first line is one column and three
        // bytes, so a byte offset would say these two are misaligned when they
        // are drawn in the same column.
        assert_eq!(
            column_of(&rows[2], 'c'),
            column_of(&rows[1], 'f'),
            "the continuation is not under the title\n{rows:#?}"
        );
        // and what still does not fit says so, rather than stopping silently
        assert!(rows[2].ends_with('…'), "{rows:#?}");
    }

    /// Where a character falls on screen, which is its character index and
    /// never its byte one.
    fn column_of(row: &str, ch: char) -> Option<usize> {
        row.chars().position(|found| found == ch)
    }

    #[test]
    fn a_title_that_fits_on_one_line_leaves_the_second_blank() {
        let mut app = App::offering(first_of_each());
        app.add(TestPane::new("s1".into(), Kind::Claude, "why".into()));
        app.cursor = 0;
        let rows = rows_of(&app, 40);
        assert!(rows[1].contains("why"), "{rows:#?}");
        // The cursor's bar is the one thing on it: a selected row is one block
        // down the rail's left edge rather than a marked line with an unmarked
        // one under it.
        assert_eq!(rows[2].trim_end(), "▌", "{rows:#?}");
    }

    #[test]
    fn the_rail_scrolls_to_wherever_the_cursor_is() {
        // `rail_cursor_line` is arithmetic over the shape `rail_lines` builds,
        // and the two would drift apart in silence — a rail that scrolled to the
        // wrong line looks like a cursor that vanished. This is what pins them.
        let mut app = App::offering(first_of_each());
        for index in 0..4 {
            app.add(TestPane::new(
                format!("s{index}"),
                Kind::Claude,
                format!("session number {index}"),
            ));
        }
        for cursor in 0..app.rows() {
            app.cursor = cursor;
            let rows = rows_of(&app, 40);
            let at = rail_cursor_line(&app);
            let expected = match app.row() {
                Some(Row::Session(index)) => format!("session number {index}"),
                Some(Row::Offer(index)) => app.offers[index].name(),
                None => unreachable!(),
            };
            assert!(
                rows[at].contains(&expected),
                "line {at} is {:?}, not the row for cursor {cursor}\n{rows:#?}",
                rows[at]
            );
        }
    }

    #[test]
    fn the_tag_is_dropped_on_a_narrow_rail_and_the_indent_is_not() {
        // Twenty-two is `rail_width`'s floor, where "(subagent)" beside a title
        // clipped to nothing would identify neither.
        let rows = rows_of(&with_a_subagent(), 22);
        let child = rows
            .iter()
            .find(|row| row.contains("codex"))
            .cloned()
            .unwrap_or_default();
        assert!(child.contains('↳'), "{child}");
        assert!(!child.contains("(subagent)"), "{child}");
    }

    #[test]
    fn an_idle_subagent_is_dimmed_and_a_failed_one_keeps_its_colour() {
        let theme = Theme::with_depth(Depth::TrueColor);
        let chrome = Chrome {
            theme,
            unicode: true,
            repo: None,
        };
        let mut app = with_a_subagent();

        // Line 0 is the SESSIONS heading and a session is two lines, so the
        // parent's first is 1 and the child's is 3. Span 1 of a first line is
        // the state mark.
        let quiet = rail_lines(&app, chrome, 40);
        assert_eq!(quiet[3].spans[1].style, theme.style(Role::Muted));
        // The parent beside it is untouched: only a *child* recedes.
        assert_eq!(quiet[1].spans[1].style, theme.style(Role::Ok));

        // A subagent that failed is not something to push into the background.
        app.panes[1].troubled = true;
        let failed = rail_lines(&app, chrome, 40);
        assert_eq!(failed[3].spans[1].style, theme.style(Role::Danger));
    }

    #[test]
    fn the_rail_separates_what_is_running_from_what_could_be_started() {
        let app = App::offering(first_of_each());
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
        let app = App::offering(vec![SignedIn::new(
            Account::new(Kind::Claude, 1, None),
            Some("ada@clubria.com".into()),
        )]);
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
        let mut app = App::offering(vec![SignedIn::new(
            Account::new(Kind::Claude, 2, None),
            Some("ada@clubria.com".into()),
        )]);
        app.begin("s1".into(), &Account::new(Kind::Claude, 2, None));
        let status = text_of(&status_line(&app, Theme::plain(), 60));
        assert!(status.starts_with("claude-2 · ada@clubria.com"), "{status}");
    }

    #[test]
    fn a_sign_in_with_no_known_address_claims_none() {
        // Codex and Grok Build keep no address riabuild can read, and a Claude
        // session can outlive the sign-in it was made under. Either way the
        // status line names the sign-in and invents nothing beside it.
        let mut app = App::offering(first_of_each());
        app.begin("s1".into(), &Account::new(Kind::Claude, 2, None));
        let status = text_of(&status_line(&app, Theme::plain(), 60));
        assert_eq!(status.trim(), "claude-2");
    }

    #[test]
    fn an_offer_says_what_typing_would_start_rather_than_waiting_for_a_reply() {
        // A pane with no session behind it once said it was waiting for a
        // reply, when there was nothing to wait for.
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
        let mut app = App::offering(first_of_each());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        app.cursor = 0;
        app.focus = Focus::List;
        let listing = text_of(&footer_line(&app, Theme::plain(), 120));
        assert!(listing.contains("move"), "{listing}");
        assert!(listing.contains("quit"), "{listing}");

        app.focus = Focus::Session;
        let writing = text_of(&footer_line(&app, Theme::plain(), 120));
        assert!(writing.contains("send"), "{writing}");
        assert!(writing.contains("scroll"), "{writing}");
        // No letter is advertised while typing: each one is a character then.
        assert!(!writing.contains("quit"), "{writing}");
    }

    #[test]
    fn a_half_written_prompt_survives_a_trip_to_the_rail() {
        let mut app = App::offering(first_of_each());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        app.focus = Focus::Session;
        for ch in "hello".chars() {
            app.compose.insert(ch);
        }
        assert!(text_of(&compose_lines(&app, Theme::plain(), true, 40)[0]).contains("hello"));
        app.focus = Focus::List;
        let from_rail = text_of(&compose_lines(&app, Theme::plain(), true, 40)[0]);
        assert!(from_rail.contains("hello"), "{from_rail}");
        // and an empty box says how to reach it rather than nothing at all
        app.compose.take();
        assert!(text_of(&compose_lines(&app, Theme::plain(), true, 40)[0]).contains("press →"));
    }

    #[test]
    fn the_caret_never_pushes_a_row_past_the_box_it_is_drawn_in() {
        // The caret is an extra cell rather than a styled one, so a row
        // wrapped right up to the box's own edge used to need one column more
        // than the box had the instant the caret landed on it — and ratatui
        // does not wrap a `Line` that runs past its `Rect`, it stops drawing,
        // so that column silently never appeared. That read as a cursor that
        // vanished on some keystrokes and a phantom blank cell on others.
        let mut app = App::offering(first_of_each());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        app.focus = Focus::Session;
        let width = 12u16;
        // Filled to exactly the width the box wraps at, with the caret at the
        // very end of it — the one position that used to overflow.
        for _ in 0..compose_wrap_width(width) {
            app.compose.insert('a');
        }
        for line in compose_lines(&app, Theme::plain(), true, width) {
            let total: usize = line
                .spans
                .iter()
                .map(|span| span.content.chars().count())
                .sum();
            assert!(
                total <= width as usize,
                "{total} > {width}: {:?}",
                text_of(&line)
            );
        }
    }

    #[test]
    fn new_session_lists_exactly_what_is_signed_in() {
        // Two Claude sign-ins and one Grok, and nothing else: no Codex row,
        // because no Codex sign-in is signed in.
        let app = App::offering(vec![
            SignedIn::new(Account::new(Kind::Claude, 1, None), None),
            SignedIn::new(Account::new(Kind::Claude, 3, None), None),
            SignedIn::new(Account::new(Kind::Grok, 2, None), None),
        ]);
        let offers: Vec<String> = rail_lines(&app, plain_chrome(), 30)
            .iter()
            .map(text_of)
            .filter(|row| row.contains("+ "))
            .collect();
        assert_eq!(offers.len(), 3, "{offers:#?}");
        for (row, name) in offers.iter().zip(["claude-1", "claude-3", "grok-2"]) {
            assert!(row.contains(name), "{row}");
        }
    }

    #[test]
    fn new_session_says_it_is_still_asking_rather_than_that_nothing_is_there() {
        let rows: Vec<String> = rail_lines(&App::new(), plain_chrome(), 30)
            .iter()
            .map(text_of)
            .collect();
        assert!(rows.iter().any(|row| row.contains("checking")), "{rows:#?}");
        assert!(!rows.iter().any(|row| row.contains("Nothing")), "{rows:#?}");
    }

    #[test]
    fn a_machine_with_nothing_signed_in_says_how_to_sign_in_outside_the_window() {
        let app = App::offering(Vec::new());
        let rows: Vec<String> = rail_lines(&app, plain_chrome(), 30)
            .iter()
            .map(text_of)
            .collect();
        assert!(!rows.iter().any(|row| row.contains("+ ")), "{rows:#?}");
        let said = rows.join(" ");
        assert!(said.contains("Nothing is signed in."), "{said}");
        assert!(said.contains("outside riabuild agents"), "{said}");
        for command in ["`claude-1 auth login`", "`codex-1 login`", "`grok-1 login`"] {
            assert!(said.contains(command), "{command} {rows:#?}");
        }
        // and every row fits the rail, a command left whole rather than split
        for row in &rows {
            assert!(row.chars().count() <= 30, "{row:?}");
        }
    }

    /// The indicator for the selected session, as plain text.
    fn indicator(app: &App, unicode: bool, width: u16) -> Option<String> {
        let chrome = Chrome {
            unicode,
            ..plain_chrome()
        };
        activity_line(app.selected(), chrome, 0, width).map(|line| text_of(&line))
    }

    fn asked(kind: Kind) -> App {
        let mut app = App::offering(first_of_each());
        app.begin("s1".into(), &Account::new(kind, 1, None));
        app.sent("fix the flaky login test");
        app
    }

    #[test]
    fn nothing_says_it_is_waiting_for_a_first_reply() {
        // A session with nothing in it yet has an empty transcript; what its
        // turn is doing is the indicator's to say, and only while it is true.
        let mut app = App::offering(first_of_each());
        app.begin("s1".into(), &Account::new(Kind::Claude, 1, None));
        assert!(transcript_lines(app.selected(), Theme::plain(), true).is_empty());
        assert_eq!(indicator(&app, true, 80), None);
    }

    #[test]
    fn a_turn_that_has_said_nothing_is_launching_its_harness() {
        for (kind, word) in [
            (Kind::Claude, "claude"),
            (Kind::Codex, "codex"),
            (Kind::Grok, "grok"),
        ] {
            let app = asked(kind);
            assert_eq!(
                indicator(&app, true, 80).as_deref(),
                Some(format!("⠋ launching {word} session…").as_str())
            );
        }
    }

    #[test]
    fn thinking_names_the_harness_the_model_and_the_effort_it_reported() {
        let mut app = asked(Kind::Claude);
        app.set_running("s1", true);
        app.observe(
            "s1",
            &riabuild_harness::Event::Ready {
                thread: Some("t".into()),
                model: Some("claude-opus-5[1m]".into()),
                effort: Some("xhigh".into()),
            },
        );
        assert_eq!(
            indicator(&app, true, 80).as_deref(),
            Some("⠋ thinking… · claude opus-5 at xhigh")
        );
    }

    #[test]
    fn what_the_harness_did_not_report_is_left_out() {
        // Codex's stream names no model: the line says so by saying less,
        // never by naming a default riabuild believes Codex has.
        let mut app = asked(Kind::Codex);
        app.set_running("s1", true);
        for event in testing::decode(Kind::Codex, r#"{"type":"thread.started","thread_id":"t1"}"#) {
            app.observe("s1", &event);
        }
        assert_eq!(
            indicator(&app, true, 80).as_deref(),
            Some("⠋ thinking… · codex")
        );
        // and a model with no effort drops only the effort
        app.panes[0].model = Some("gpt-5.6-sol".into());
        assert_eq!(
            indicator(&app, true, 80).as_deref(),
            Some("⠋ thinking… · codex gpt-5.6-sol")
        );
    }

    #[test]
    fn a_tool_in_flight_is_named_with_what_it_is_running() {
        let mut app = asked(Kind::Claude);
        app.set_running("s1", true);
        // The canned turn up to its tool call: the call is still out.
        let first_two: Vec<&str> = testing::CLAUDE.lines().take(2).collect();
        for event in testing::decode(Kind::Claude, &first_two.join("\n")) {
            app.observe("s1", &event);
        }
        let theme = Theme::with_depth(Depth::TrueColor);
        let chrome = Chrome {
            theme,
            ..plain_chrome()
        };
        let line = activity_line(app.selected(), chrome, 0, 80).unwrap();
        assert_eq!(text_of(&line), "⠋ running Bash  cargo test --workspace");
        // in the working colour, so it does not read as the agent thinking
        assert_eq!(line.spans[1].style, theme.style(Role::Busy));
        assert_eq!(line.spans[2].style, theme.style(Role::Strong));

        // and once the result is in, it is back to thinking
        let rest: Vec<&str> = testing::CLAUDE.lines().skip(2).take(2).collect();
        for event in testing::decode(Kind::Claude, &rest.join("\n")) {
            app.observe("s1", &event);
        }
        assert!(
            indicator(&app, true, 80)
                .unwrap()
                .starts_with("⠋ thinking… · claude opus-5")
        );
    }

    #[test]
    fn a_session_whose_turn_is_over_draws_no_indicator_at_all() {
        let mut app = asked(Kind::Claude);
        app.set_running("s1", true);
        for event in testing::decode(Kind::Claude, testing::CLAUDE) {
            app.observe("s1", &event);
        }
        app.set_running("s1", false);
        assert_eq!(indicator(&app, true, 80), None);
    }

    #[test]
    fn the_indicator_has_an_ascii_spelling_and_fits_its_row() {
        let app = asked(Kind::Grok);
        assert_eq!(
            indicator(&app, false, 80).as_deref(),
            Some("~ launching grok session...")
        );
        let cut = indicator(&app, true, 12).unwrap();
        assert!(cut.chars().count() <= 12, "{cut}");
        assert!(cut.starts_with("⠋ "), "{cut}");
    }
}
