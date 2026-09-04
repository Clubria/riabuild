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
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, Pane, Row};
use crate::draw::Chrome;
use crate::paste::{self, Pasted};
use crate::store::{self, Store};
use crate::{Action, Login, Request, Screen, frame, key, keys};

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
    for record in store.sessions(&request.cwd).await.unwrap_or_default() {
        let Some(kind) = record.harness() else {
            continue;
        };
        // Replayed through the same decoder a live turn is read with, so a
        // reopened pane shows what was on screen when the work happened rather
        // than a reconstruction of it.
        let mut reader = Reader::new(kind);
        let spool = store.spool(&record.id).await.unwrap_or_default();
        let (trouble, trouble_at) = store.trouble_since(&record.id, 0).await.unwrap_or_default();
        let running = store.running(&record.id).await;

        // A session an older riabuild opened on the way in and nobody ever
        // spoke to. Nothing was said, nothing failed and nothing is running, so
        // there is no conversation to lose — and leaving them listed would carry
        // the bug this redesign removes onto every machine that already has
        // three of them per checkout.
        if record.title.is_empty() && spool.is_empty() && trouble.is_empty() && !running {
            let _ = store.forget(&record.id).await;
            continue;
        }

        let mut pane = Pane::new(record.id.clone(), kind, record.title.clone());
        pane.thread = record.thread.clone();
        pane.account = record.account;
        pane.offset = spool.len() as u64;
        for line in spool.lines() {
            for event in reader.read(line) {
                pane.observe(&event);
            }
        }
        // Replayed like the spool, so a failure from yesterday's turn is still
        // on screen when the window comes back.
        for line in trouble.lines().filter(|line| !line.trim().is_empty()) {
            pane.observe(&riabuild_harness::Event::Trouble(line.to_string()));
        }
        pane.trouble_offset = trouble_at;
        pane.running = running;
        readers.insert(record.id.clone(), reader);
        app.add(pane);
    }
    app.cursor = 0;
    Ok(readers)
}

/// `riabuild agents "do the thing"` — asked of every harness at once.
///
/// One session per offer, created here rather than on the way in, because this
/// is a prompt: it is the thing that turns an offer into a session everywhere
/// else too.
pub async fn first_prompt(
    store: &Store,
    runner: &dyn CommandRunner,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
    prompt: &str,
) {
    let sessions = app.panes.len();
    for offer in 0..app.offers.len() {
        app.cursor = sessions + offer;
        send(store, runner, request, app, readers, prompt).await;
    }
    app.cursor = 0;
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
    mut logins: UnboundedReceiver<Login>,
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
            // Answers arrive over the life of the window rather than before it
            // opens: asking who is signed in is a subprocess per account.
            Some(login) = logins.recv() => {
                app.set_login(login.kind, login.number, login.email);
                Action::Nothing
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                follow(store, app, readers).await;
                Action::Nothing
            }
        };

        match action {
            Action::Nothing => {}
            Action::Quit => app.quit = true,
            Action::Send(text) => send(store, reach.runner, request, app, readers, &text).await,
            Action::Paste => paste_into_compose(store, app, reach.clipboard).await,
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
                let events: Vec<_> = fresh.lines().flat_map(|line| reader.read(line)).collect();
                for event in events {
                    app.observe(&id, &event);
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
            if let Some(pane) = app.pane_mut(&id) {
                pane.trouble_offset = at;
            }
        }
        // Asked every tick rather than inferred from the stream: a turn can also
        // end by being killed, and nothing is written then.
        let running = store.running(&id).await;
        app.set_running(&id, running);
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
                    record.id
                }
                // Nowhere to write this: there is no session yet, so there is no
                // `errors.log` for it either. The offer stays on the rail and
                // the next prompt tries again.
                Err(_) => return,
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
        app.observe(&id, &riabuild_harness::Event::Trouble(format!("{error:#}")));
        app.set_running(&id, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, Accounts};
    use riabuild_harness::Kind;
    use riabuild_runner::FakeRunner;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn request(root: &std::path::Path) -> Request {
        Request {
            riabuild: PathBuf::from("/opt/riabuild"),
            cwd: root.join("checkout"),
            repo: Some("Clubria/riabuild".into()),
            accounts: Accounts::from(vec![Account::new(Kind::Claude, 1, None)]),
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
        let mut app = App::new(request.accounts.clone());
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

        let mut app = App::new(request.accounts.clone());
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

        let mut app = App::new(request.accounts.clone());
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
        let mut app = App::new(Accounts::from(vec![Account::new(Kind::Claude, 1, None)]));
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
        let mut app = App::new(Accounts::from(vec![Account::new(Kind::Claude, 1, None)]));
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
        let mut app = App::new(Accounts::from(vec![Account::new(Kind::Claude, 1, None)]));

        paste_into_compose(&store, &mut app, None).await;
        let notice = app.notice.unwrap_or_default();
        assert!(
            notice.contains("xclip") || notice.contains("wl-clipboard"),
            "{notice}"
        );
    }
}
