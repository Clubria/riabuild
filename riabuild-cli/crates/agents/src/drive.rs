//! The loop, and the four things it does between frames.
//!
//! Split out of `lib.rs` so that file is the keymap and the terminal and
//! nothing else. Everything here touches the store; nothing here draws.

use std::collections::HashMap;

use anyhow::Result;
use ratatui::crossterm::event::Event as TermEvent;
use riabuild_channel::clipboard::Clipboard;
use riabuild_harness::Reader;
use riabuild_runner::CommandRunner;
use tokio::sync::oneshot;

use crate::account::SignedIn;
use crate::app::{App, Pane, Row};
use crate::draw::Chrome;
use crate::paste::{self, Pasted};
use crate::store::{self, Store};
use crate::{Action, Request, Screen, frame, key, keys};

/// How many ticks apart the window looks for sessions it did not open.
///
/// Twenty-five of the 120ms tick below, so about three seconds. `follow` reads
/// two files per pane and is cheap enough for every tick; this is a `read_dir`
/// and a small read per session, which is not — and what it is watching for is
/// a subagent *appearing*, where three seconds is imperceptible. Its output is
/// live from the moment it does, because `follow` picks the pane up on the very
/// next tick.
const RESCAN_TICKS: usize = 25;

/// Loads this checkout's sessions and replays what they have already said.
///
/// It creates nothing. The window used to open a session per harness here, so a
/// developer who had never typed anything still had three directories on disk
/// and a header that said "3 sessions" — and the three sign-ins those panes
/// stood for are offers on the rail now, which cost nothing to show.
pub async fn restore(
    store: &Store,
    request: &Request,
    app: &mut App,
) -> Result<HashMap<String, Reader>> {
    let mut readers = HashMap::new();
    // Grouped before anything is drawn, because the rail says "this pane is a
    // child of the one above it" by indenting it — a claim only the *order* can
    // make true. Sorting at draw time instead would leave `App::cursor`, which
    // indexes `App::panes` directly, selecting whichever session happened to sit
    // at that position before the rearrangement.
    for record in crate::store::arrange(store.sessions(&request.cwd).await.unwrap_or_default()) {
        let Some((pane, reader)) = hydrate(store, &record).await else {
            // A session an older riabuild opened on the way in and nobody ever
            // spoke to. Nothing was said, nothing failed and nothing is
            // running, so there is no conversation to lose — and leaving them
            // listed would carry the bug this redesign removes onto every
            // machine that already has three of them per checkout.
            //
            // Only *here*. `adopt` meets the same shape a few milliseconds
            // after `riabuild internal mcp-codex` made the directory and before
            // its first turn has written a byte, and deleting that would take
            // the session out from under a delegation that is already running.
            let _ = store.forget(&record.id).await;
            continue;
        };
        readers.insert(record.id.clone(), reader);
        app.add(pane);
    }
    app.cursor = 0;
    Ok(readers)
}

/// One record, read off the disk and replayed into a pane.
///
/// `None` for a session nobody has spoken to — no title, no spool, no failure
/// and no turn holding its lock. What the caller does about that differs, which
/// is why this reports it rather than deciding: [`restore`] forgets it, and
/// [`adopt`] leaves it alone for the next rescan.
async fn hydrate(store: &Store, record: &store::Record) -> Option<(Pane, Reader)> {
    let kind = record.harness()?;
    // Replayed through the same decoder a live turn is read with, so a reopened
    // pane shows what was on screen when the work happened rather than a
    // reconstruction of it.
    let mut reader = Reader::new(kind);
    let spool = store.spool(&record.id).await.unwrap_or_default();
    let (trouble, trouble_at) = store.trouble_since(&record.id, 0).await.unwrap_or_default();
    let running = store.running(&record.id).await;

    if record.title.is_empty() && spool.is_empty() && trouble.is_empty() && !running {
        return None;
    }

    let mut pane = Pane::new(record.id.clone(), kind, record.title.clone());
    pane.thread = record.thread.clone();
    pane.parent = record.parent.clone();
    pane.account = record.account;
    pane.offset = spool.len() as u64;
    for line in spool.lines() {
        let events = reader.read(line);
        if events.is_empty() && !line.trim().is_empty() {
            pane.turn.heard(pane.entries.len());
        }
        for event in events {
            pane.observe(&event);
        }
    }
    // Replayed like the spool, so a failure from yesterday's turn is still on
    // screen when the window comes back.
    for line in trouble.lines().filter(|line| !line.trim().is_empty()) {
        pane.observe(&riabuild_harness::Event::Trouble(line.to_string()));
    }
    if !trouble.trim().is_empty() {
        pane.turn.stopped();
    }
    pane.trouble_offset = trouble_at;
    // Through the setter, so a turn that is running as the window opens is
    // seen launching or mid-stream according to what the spool already holds.
    pane.set_running(running);
    Some((pane, reader))
}

/// Puts sessions this window did not open onto the rail.
///
/// There is exactly one thing that makes them: `riabuild internal mcp-codex`,
/// started by a Claude Code session inside this window, which creates a Codex
/// session of its own and runs a turn in it. Without this the developer would
/// watch Claude sit on a tool call for two minutes with nothing to look at, and
/// the subagent would appear only the next time the window was opened — which
/// is precisely when its output has stopped being interesting.
///
/// Every rescan re-groups, because a child that arrives has to land under its
/// parent rather than at the end of the list.
async fn adopt(
    store: &Store,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
) {
    let records = store.sessions(&request.cwd).await.unwrap_or_default();
    let known: std::collections::HashSet<String> =
        app.panes.iter().map(|pane| pane.id.clone()).collect();
    let mut arrived = false;
    for record in &records {
        if known.contains(&record.id) {
            continue;
        }
        let Some((pane, reader)) = hydrate(store, record).await else {
            continue;
        };
        readers.insert(record.id.clone(), reader);
        app.add(pane);
        arrived = true;
    }
    if arrived {
        regroup(app, &records);
    }
}

/// Puts the panes back in `store::arrange` order, keeping the cursor where the
/// developer left it.
///
/// By id and never by index: the whole point of this function is that indices
/// have just changed, and a cursor restored to its old number would select
/// whatever moved into that row.
fn regroup(app: &mut App, records: &[store::Record]) {
    let held = match app.row() {
        Some(Row::Session(index)) => app.panes.get(index).map(|pane| pane.id.clone()),
        _ => None,
    };
    let offered = matches!(app.row(), Some(Row::Offer(_)));
    let offer_at = match app.row() {
        Some(Row::Offer(index)) => index,
        _ => 0,
    };

    let order: Vec<String> = crate::store::arrange(records.to_vec())
        .into_iter()
        .map(|record| record.id)
        .collect();
    // A pane with no record — one created a moment ago by this window's own
    // `begin` and not yet on disk when the listing was taken — is the newest
    // there is, so it stays at the top where `begin` put it rather than being
    // dropped. `None` sorts before `Some`, and `sort_by_key` is stable, so
    // several of them keep the order they were added in.
    app.panes
        .sort_by_key(|pane| order.iter().position(|id| id == &pane.id));

    if let Some(id) = held {
        if let Some(at) = app.panes.iter().position(|pane| pane.id == id) {
            app.cursor = at;
        }
    } else if offered {
        // An offer is identified by its position in a list this never touches,
        // but the *rail* puts the offers after the sessions — so a session
        // arriving moves every offer down one, and a cursor left where it was
        // would jump to a different sign-in.
        app.cursor = app.panes.len() + offer_at;
    }
}

/// `riabuild agents "do the thing"` — asked of every harness at once.
///
/// One session per harness that has anything signed in, under the first of its
/// signed-in sign-ins, created here rather than on the way in, because this is
/// a prompt: it is the thing that turns an offer into a session everywhere else
/// too.
pub async fn first_prompt(
    store: &Store,
    runner: &dyn CommandRunner,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
    prompt: &str,
) {
    let firsts: Vec<usize> = (0..app.offers.len())
        .filter(|&at| {
            let kind = app.offers[at].kind;
            app.offers[..at].iter().all(|earlier| earlier.kind != kind)
        })
        .collect();
    for offer in firsts {
        // Offers come after sessions, and each send adds one.
        app.move_to(app.panes.len() + offer);
        send(store, runner, request, app, readers, prompt).await;
    }
    app.move_to(0);
}

/// Everything outside the window it can reach while it is open.
///
/// Grouped rather than passed one by one because they are the same kind of
/// thing — the two ways this crate touches the machine it is running on — and
/// because a loop with eight parameters is one nobody can add the ninth to.
#[derive(Clone, Copy)]
pub struct Reach<'a> {
    /// How every external process is started, without exception.
    pub runner: &'a dyn CommandRunner,
    /// What Ctrl-V reads. `None` is a Linux laptop with no clipboard tool
    /// installed, which is a notice rather than a window that will not open.
    pub clipboard: Option<&'a dyn Clipboard>,
}

pub async fn drive(
    terminal: &mut Screen,
    store: &Store,
    reach: Reach<'_>,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
    mut signed_in: oneshot::Receiver<Vec<SignedIn>>,
) -> Result<()> {
    let mut keys = keys();
    // Fast enough for the spinner to read as motion and for output to feel live,
    // slow enough that an idle window is not reading three files a hundred times
    // a second.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(120));
    let chrome = Chrome {
        theme: request.theme,
        unicode: request.unicode,
        repo: request.repo.as_deref(),
    };

    loop {
        terminal.draw(|f| frame::render(f, app, chrome))?;
        if app.quit {
            return Ok(());
        }

        let action = tokio::select! {
            Some(event) = keys.recv() => match event {
                TermEvent::Key(pressed) => key(app, pressed),
                _ => Action::Nothing,
            },
            // Arrives once, a moment after the window opens: asking Claude
            // Code who is signed in is a subprocess per account. Not polled
            // again once it has answered, which a oneshot does not allow.
            found = &mut signed_in, if app.checking => {
                app.signed_in(found.unwrap_or_default());
                Action::Nothing
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                app.expire_quit(std::time::Instant::now());
                follow(store, app, readers).await;
                if app.tick.is_multiple_of(RESCAN_TICKS) {
                    adopt(store, request, app, readers).await;
                }
                Action::Nothing
            }
        };

        match action {
            Action::Nothing => {}
            Action::Quit => app.quit = true,
            Action::Send(text) => send(store, reach.runner, request, app, readers, &text).await,
            Action::Paste => paste_into_compose(store, app, reach.clipboard).await,
            Action::Interrupt => interrupt(store, app).await,
        }
    }
}

/// Ctrl-V: what the clipboard holds, in the box.
///
/// Every way this can fail is a notice rather than an error returned. The
/// window is the developer's session with three agents; a clipboard tool that
/// would not run is not a reason to close it, and the message is one keypress
/// from being gone.
async fn paste_into_compose(store: &Store, app: &mut App, clipboard: Option<&dyn Clipboard>) {
    let Some(clipboard) = clipboard else {
        // Named rather than described, the way `install_hint` is: "paste does
        // not work" is not something a developer can act on.
        app.notice = Some(riabuild_channel::clipboard::install_hint_for_this_machine().to_string());
        return;
    };
    match paste::read(clipboard, &store.images_dir()).await {
        Ok(Pasted::Image(path)) => {
            // The path, in the line, as text the developer can see and edit —
            // there is no hidden attachment list for a backspace to
            // desynchronise. A space after it because the next thing typed is a
            // sentence about the image.
            for ch in path.display().to_string().chars() {
                app.compose.insert(ch);
            }
            app.compose.insert(' ');
        }
        Ok(Pasted::Text(text)) => {
            for ch in text.chars() {
                app.compose.insert(ch);
            }
        }
        Ok(Pasted::Nothing) => app.notice = Some("Nothing on the clipboard to paste.".to_string()),
        Err(error) => app.notice = Some(format!("{error:#}")),
    }
}

/// Esc, on a session that is still working: asks its turn to stop.
///
/// Fire-and-forget, like [`paste_into_compose`]'s clipboard read — there is
/// nothing to wait for here either, because the turn this asks is a detached
/// process this window never held a handle to (see the crate's own doc
/// comment on why). `follow`'s next tick is what tells the pane the turn is
/// actually gone, the same way it always learns a turn ended.
async fn interrupt(store: &Store, app: &App) {
    let Some(pane) = app.selected() else { return };
    let _ = store.request_cancel(&pane.id).await;
}

/// Reads whatever the running turns have appended since the last tick.
async fn follow(store: &Store, app: &mut App, readers: &mut HashMap<String, Reader>) {
    let ids: Vec<(String, u64, u64)> = app
        .panes
        .iter()
        .map(|pane| (pane.id.clone(), pane.offset, pane.trouble_offset))
        .collect();
    for (id, offset, trouble_at) in ids {
        if let Ok((fresh, moved)) = store.spool_since(&id, offset).await
            && !fresh.is_empty()
        {
            if let Some(reader) = readers.get_mut(&id) {
                for line in fresh.lines() {
                    let events = reader.read(line);
                    // A line that decodes to nothing — a hook starting, a
                    // frame this riabuild does not know — is still the harness
                    // speaking, so the turn is past launching.
                    if events.is_empty() && !line.trim().is_empty() {
                        app.heard(&id);
                    }
                    for event in events {
                        app.observe(&id, &event);
                    }
                }
            }
            if let Some(pane) = app.pane_mut(&id) {
                pane.offset = moved;
            }
        }
        // riabuild's own failures, which have nowhere in the harness's stream to
        // live: a binary that would not start writes here and nowhere else.
        if let Ok((trouble, at)) = store.trouble_since(&id, trouble_at).await
            && !trouble.is_empty()
        {
            for line in trouble.lines().filter(|line| !line.trim().is_empty()) {
                app.observe(&id, &riabuild_harness::Event::Trouble(line.to_string()));
            }
            // Written as a turn ends and never during one, so whatever was on
            // the indicator is over.
            app.stopped(&id);
            if let Some(pane) = app.pane_mut(&id) {
                pane.trouble_offset = at;
            }
        }
        // Asked every tick rather than inferred from the stream: a turn can also
        // end by being killed, and nothing is written then.
        let running = store.running(&id).await;
        app.set_running(&id, running);
        look_up_codex_turn(store, app, &id).await;
    }
}

/// Fills in a Codex turn's model and effort from its rollout.
///
/// The one harness whose stream carries neither — see `crate::rollout`. Looked
/// up a few times per turn at most, and only once the thread id is known,
/// because the file is named after it.
async fn look_up_codex_turn(store: &Store, app: &mut App, id: &str) {
    let Some(pane) = app.pane_mut(id) else {
        return;
    };
    if pane.kind != riabuild_harness::Kind::Codex || !pane.turn.wants_lookup() {
        return;
    }
    let Some(thread) = pane.thread.clone() else {
        return;
    };
    pane.turn.lookups -= 1;
    let known = pane.turn.rollout.clone();

    let path = match known {
        Some(path) => Some(path),
        None => match store.read(id).await.ok().and_then(|record| record.home) {
            Some(home) => crate::rollout::find(&home, &thread).await,
            None => None,
        },
    };
    let Some(path) = path else {
        return;
    };
    let context = crate::rollout::read(&path).await;
    let Some(pane) = app.pane_mut(id) else {
        return;
    };
    pane.turn.rollout = Some(path);
    if let Some(context) = context {
        pane.turn.lookups = 0;
        pane.model = context.model;
        pane.effort = context.effort;
    }
}

/// Starts a turn for whatever the cursor is on, creating the session if the
/// cursor is on an offer.
///
/// This is the one place a session comes into existence, and that is the whole
/// of the answer to "why did a window that had been asked nothing say three".
pub async fn send(
    store: &Store,
    runner: &dyn CommandRunner,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
    text: &str,
) {
    // Set where this prompt is what created the session, so a first turn that
    // will not start can take the session back rather than leave an empty one.
    let mut created = None;
    let id = match app.row() {
        Some(Row::Session(index)) => match app.panes.get(index) {
            Some(pane) => pane.id.clone(),
            None => return,
        },
        Some(Row::Offer(index)) => {
            let Some(account) = app.offers.get(index).cloned() else {
                return;
            };
            match store.create(&account, &request.cwd).await {
                Ok(record) => {
                    readers.insert(record.id.clone(), Reader::new(account.kind));
                    app.begin(record.id.clone(), &account);
                    created = Some(account);
                    record.id
                }
                // Nowhere to write this: there is no session yet, so there is no
                // `errors.log` for it either. It used to be dropped here, with
                // the prompt already out of the box — so Enter did nothing,
                // said nothing, and lost what was typed. The offer's pane says
                // why instead, and the prompt goes back in its box.
                Err(error) => {
                    app.failed(
                        &account,
                        format!("could not create the session: {error:#}"),
                        text,
                    );
                    return;
                }
            }
        }
        None => return,
    };
    app.sent(text);

    // Re-read rather than trusting what this window remembers: a turn started
    // from another window may have learned the thread id since, and resuming
    // without it starts a second conversation instead of continuing this one.
    let record = match store.read(&id).await {
        Ok(mut record) => {
            if record.title.is_empty() {
                record.title = store::title_of(text);
                let _ = store.write(&record).await;
            }
            record
        }
        Err(error) => {
            app.observe(&id, &riabuild_harness::Event::Trouble(format!("{error:#}")));
            app.stopped(&id);
            app.set_running(&id, false);
            return;
        }
    };

    // A turn that will not start is this session's problem and not the window's:
    // the other agents keep running and the pane says what happened.
    if let Err(error) = store
        .start_turn(runner, &request.riabuild, &record, text)
        .await
    {
        match created {
            // The session's very first turn: nothing ever ran in it, so it is
            // not a conversation. It goes, and the offer it came from says why.
            Some(account) => {
                readers.remove(&id);
                let _ = store.forget(&id).await;
                app.abandon(
                    &id,
                    &account,
                    format!("could not start the session: {error:#}"),
                    text,
                );
            }
            None => {
                app.observe(&id, &riabuild_harness::Event::Trouble(format!("{error:#}")));
                app.stopped(&id);
                app.set_running(&id, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;
    use crate::account::tests::first_of_each;
    use riabuild_harness::Kind;
    use riabuild_runner::FakeRunner;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn request(root: &std::path::Path) -> Request {
        Request {
            riabuild: PathBuf::from("/opt/riabuild"),
            cwd: root.join("checkout"),
            repo: Some("Clubria/riabuild".into()),
            prompt: None,
            theme: riabuild_theme::Theme::plain(),
            unicode: true,
        }
    }

    #[tokio::test]
    async fn a_prompt_is_what_creates_a_session_on_disk() {
        // The bug, from the other end: browsing the rail must leave the
        // filesystem exactly as it was found.
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let request = request(temp.path());
        let runner = Arc::new(FakeRunner::new());
        let mut app = App::offering(first_of_each());
        let mut readers = restore(&store, &request, &mut app).await.unwrap();
        assert!(app.panes.is_empty());
        assert_eq!(store.sessions(&request.cwd).await.unwrap().len(), 0);

        // The cursor is on the first offer once the rail has no sessions.
        assert!(app.offered().is_some());
        send(
            &store,
            runner.as_ref(),
            &request,
            &mut app,
            &mut readers,
            "why is the nightly job slow",
        )
        .await;
        assert_eq!(app.panes.len(), 1);
        let sessions = store.sessions(&request.cwd).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].title,
            store::title_of("why is the nightly job slow")
        );
        // and the cursor followed the session it just made
        assert!(app.selected().is_some());
    }

    #[tokio::test]
    async fn a_prompt_on_the_command_line_goes_to_the_first_signed_in_sign_in_of_each_harness() {
        // Two Claude sign-ins and a Codex one, Grok Build signed out: one
        // session under `claude-1`, one under `codex-1`, and none under
        // anything that is not signed in.
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let request = request(temp.path());
        let runner = Arc::new(FakeRunner::new());
        let mut app = App::offering(vec![
            SignedIn::new(Account::new(Kind::Claude, 1, None), None),
            SignedIn::new(Account::new(Kind::Claude, 2, None), None),
            SignedIn::new(Account::new(Kind::Codex, 1, None), None),
        ]);
        let mut readers = restore(&store, &request, &mut app).await.unwrap();
        first_prompt(
            &store,
            runner.as_ref(),
            &request,
            &mut app,
            &mut readers,
            "why is the nightly job slow",
        )
        .await;
        let mut names: Vec<String> = app.panes.iter().map(Pane::account_name).collect();
        names.sort();
        assert_eq!(names, ["claude-1", "codex-1"]);
    }

    #[tokio::test]
    async fn an_untouched_session_from_an_older_riabuild_is_cleaned_up() {
        // Every existing install has three of these per checkout, made by a
        // window that opened a pane per harness. Nothing was ever said in one,
        // so there is nothing to lose by forgetting it — and listing them is
        // the "3 sessions" bug arriving on a machine that upgraded.
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let request = request(temp.path());
        for kind in Kind::ALL {
            store
                .create(&Account::new(kind, 1, None), &request.cwd)
                .await
                .unwrap();
        }
        assert_eq!(store.sessions(&request.cwd).await.unwrap().len(), 3);

        let mut app = App::offering(first_of_each());
        restore(&store, &request, &mut app).await.unwrap();
        assert!(app.panes.is_empty());
        assert_eq!(store.sessions(&request.cwd).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_session_that_was_asked_something_survives_reopening() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let request = request(temp.path());
        let mut record = store
            .create(&Account::new(Kind::Claude, 1, None), &request.cwd)
            .await
            .unwrap();
        record.title = "a real conversation".into();
        store.write(&record).await.unwrap();

        let mut app = App::offering(first_of_each());
        restore(&store, &request, &mut app).await.unwrap();
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.panes[0].label(), "a real conversation");
    }

    /// A clipboard holding one PNG, however this machine spells that.
    fn with_an_image() -> Arc<FakeRunner> {
        Arc::new(
            FakeRunner::new()
                .with(
                    "xclip -selection clipboard -t TARGETS -o",
                    0,
                    "image/png\n",
                    "",
                )
                .with_bytes(
                    "xclip -selection clipboard -t image/png -o",
                    0,
                    &[0x89, b'P', b'N', b'G'],
                    "",
                ),
        )
    }

    /// The whole feature, end to end and one layer below the terminal: a
    /// pasted image is a *file* the agent can open, and its path is in the
    /// prompt as text the developer can see and edit. There is no hidden
    /// attachment list a backspace could put out of step with the line.
    #[tokio::test]
    async fn pasting_an_image_puts_a_readable_path_in_the_box() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let mut app = App::new();
        let runner: Arc<dyn riabuild_runner::CommandRunner> = with_an_image();
        let clipboard = riabuild_channel::clipboard::CliClipboard::x11(runner);

        app.compose.insert('?');
        app.compose.start();
        paste_into_compose(&store, &mut app, Some(&clipboard)).await;

        let text = app.compose.text().to_string();
        // Inserted at the caret like anything else typed, so the character that
        // was already there is after it.
        let path = text.trim_end_matches('?').trim_end();
        assert!(
            path.starts_with(&store.images_dir().display().to_string()),
            "{text}"
        );
        assert!(tokio::fs::metadata(path).await.is_ok(), "{path}");
        assert_eq!(app.notice, None);
    }

    /// An empty clipboard is the ordinary case. It says so and takes nothing
    /// down: a key that does nothing and says nothing reads as one that is not
    /// bound at all.
    #[tokio::test]
    async fn an_empty_clipboard_is_said_out_loud_and_nothing_else() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let mut app = App::new();
        let runner: Arc<dyn riabuild_runner::CommandRunner> =
            Arc::new(FakeRunner::new().with("xclip -selection clipboard -t TARGETS -o", 1, "", ""));
        let clipboard = riabuild_channel::clipboard::CliClipboard::x11(runner);

        paste_into_compose(&store, &mut app, Some(&clipboard)).await;
        assert!(app.compose.is_empty());
        assert!(app.notice.is_some());
    }

    /// A Linux laptop with neither `xclip` nor `wl-clipboard`. The window opens
    /// and works; Ctrl-V names the package to install, because "paste does not
    /// work" is not something a developer can act on.
    #[tokio::test]
    async fn a_laptop_with_no_clipboard_tool_is_told_what_to_install() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let mut app = App::new();

        paste_into_compose(&store, &mut app, None).await;
        let notice = app.notice.unwrap_or_default();
        assert!(
            notice.contains("xclip") || notice.contains("wl-clipboard"),
            "{notice}"
        );
    }

    /// The window's half of the feature: asking is a marker on disk, not a
    /// signal to a process this window never held a handle to.
    #[tokio::test]
    async fn escape_asks_the_selected_sessions_turn_to_stop() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::rooted_at(temp.path());
        let record = store
            .create(&Account::new(Kind::Claude, 1, None), Path::new("/work"))
            .await
            .unwrap();
        let mut app = App::offering(vec![SignedIn::new(
            Account::new(Kind::Claude, 1, None),
            None,
        )]);
        app.add(Pane::new(
            record.id.clone(),
            Kind::Claude,
            "a session".into(),
        ));
        app.cursor = 0;

        assert!(!store.cancel_requested(&record.id).await);
        interrupt(&store, &app).await;
        assert!(store.cancel_requested(&record.id).await);
    }
}
