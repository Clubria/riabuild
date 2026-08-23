//! `riabuild remote` with nothing after it: which saved server, or a new one.
//!
//! Split out of `store.rs`, which owns `remotes.json` and the `[user@]host[:port]`
//! half of `store::choose`, and was already at the crate's ~300-line production
//! budget before this became a prompt rather than three behaviours chosen by how
//! many servers happened to be saved.
//!
//! The question is put in two halves for one reason: [`settle`] only decides
//! what the developer *meant*, and never acts on it. Its Add answer is
//! therefore testable without a test process ever reaching `ask_required`,
//! which reads the real stdin — under `cargo test` run from a terminal, that
//! is a blocking read on the developer's own keyboard rather than a test.

use super::store::{self, Record, Store};
use super::{Remote, render};
use anyhow::Result;
use riabuild_tasks::Ctx;
use riabuild_ui::{Failure, Ui};

/// How many unusable answers are asked about again before riabuild takes the
/// default. Bounded like `store::ask_name`, and for the same reason: a
/// developer who cannot give a usable answer is better served by riabuild
/// choosing than by being asked forever.
const ATTEMPTS: usize = 3;

/// What an answer to the picker meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Connect to a saved server, by its index in the box.
    Server(usize),
    /// Add one that is not saved yet.
    Add,
}

/// What a typed answer means, given how many servers were listed.
///
/// `None` is "that was not an answer" — the caller asks again. Pure, so the
/// rules are testable without a terminal.
pub fn parse_pick(answer: &str, count: usize) -> Option<Pick> {
    let answer = answer.trim().to_ascii_lowercase();
    if matches!(answer.as_str(), "n" | "new") {
        return Some(Pick::Add);
    }
    let number: usize = answer.parse().ok()?;
    if number == render::add_option(count) {
        return Some(Pick::Add);
    }
    // Nothing is guessed at from here: the old picker read its answer with
    // `unwrap_or(1)`, so `0` and a number past the end both connected to
    // whichever server happened to be listed first.
    (1..=count)
        .contains(&number)
        .then(|| Pick::Server(number - 1))
}

/// Which server this invocation is about, when the command line named none.
pub async fn pick(ctx: &mut Ctx, store: &mut Store) -> Result<Remote> {
    // Everything that could be connected to — which leaves out one of the
    // team's servers that this run's fetch did not describe, because its
    // address is a memory rather than somewhere to connect. A laptop whose
    // only servers are those is a laptop with nothing to pick between, and
    // falls into the add questions below exactly as an empty store does.
    let shown: Vec<Record> = store.reachable().into_iter().cloned().collect();

    // Nothing saved: there is nothing to pick between, and a one-option
    // picker is a worse way to ask for a hostname than asking for a hostname.
    if shown.is_empty() {
        return add_one(ctx, store);
    }
    if !ctx.ui.interactive() {
        return without_a_terminal(ctx, &shown);
    }

    let default = render::most_recently_used(&shown).unwrap_or(0);
    ctx.ui.info("");
    ctx.ui.info(&render::servers_box(
        &shown,
        render::Shown::Choosing,
        ctx.ui.theme(),
    ));
    match settle(&ctx.ui, &shown, default) {
        // Whatever `settle` returns is in range: it only ever reports an index
        // `parse_pick` accepted, or the default it was handed.
        Pick::Server(index) => Ok((&shown[index]).into()),
        Pick::Add => add_one(ctx, store),
    }
}

/// The answer, before anything is done about it.
///
/// Deciding and acting are separate so this half can be driven by a scripted
/// `Ui` — see the note at the foot of this file's tests.
///
/// The default is named as well as numbered, and that is not decoration:
/// `Ui::info` returns early under `--quiet` while `Ui::ask` does not, so
/// `riabuild --quiet remote` puts this question with the box above it silently
/// dropped, where a bare `[2]` refers to a row nobody was shown. The same
/// reason `accounts::command::confirm_question` names the account inside the
/// question rather than only in the lines above it.
fn settle(ui: &Ui, records: &[Record], default: usize) -> Pick {
    let count = records.len();
    let question = match records.get(default) {
        // The display name, because it is the one the developer would type and
        // the one the box above shows — and under `--quiet` the box is dropped
        // while this question is not, so it is the only identification left.
        Some(record) => format!("Which one? [{} · {}]", default + 1, record.display_name()),
        None => format!("Which one? [{}]", default + 1),
    };
    for _ in 0..ATTEMPTS {
        // `None` is Enter, ^D, or nobody there, and all three mean "the one you
        // offered" — so none of them costs the developer an attempt.
        let Some(answer) = ui.ask(&question) else {
            break;
        };
        if let Some(pick) = parse_pick(&answer, count) {
            return pick;
        }
        ui.warn(&format!(
            "Pick a number from 1 to {count}, or {} to add a server.",
            render::add_option(count)
        ));
    }
    Pick::Server(default)
}

/// Both of the answers a run with no terminal gets, unchanged from before this
/// was a prompt.
///
/// The crate rule is that a prompt always has a default, because asking is how
/// riabuild *offers* a choice. This question is the exception the rule already
/// names: connecting provisions the server, mints it a session, and lends it
/// this laptop's GitHub sign-in, so a default taken by nobody would act on a
/// machine nobody chose. One saved server is not a guess, and several are.
fn without_a_terminal(ctx: &Ctx, shown: &[Record]) -> Result<Remote> {
    if let [record] = shown {
        ctx.ui.info(&format!(
            "Reconnecting to {} · {}@{}",
            record.display_name(),
            record.user,
            record.host
        ));
        return Ok(record.into());
    }
    Err(Failure::new(
        "asking which of your servers to connect to",
        "Name it — `riabuild remote <name>` — there is no terminal here to ask in.",
    )
    .detail(format!(
        "servers riabuild could reach: {}",
        shown
            .iter()
            .map(Record::display_name)
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .into())
}

/// Adds a server, and records it.
fn add_one(ctx: &Ctx, store: &mut Store) -> Result<Remote> {
    let remote = ask_for_one(ctx, store)?;
    store::add(store, &remote);
    Ok(remote)
}

/// The questions, once, for a server riabuild has never seen.
///
/// `ask_required`, not `ask`: a hostname has no default that could be right,
/// and this is the one place in riabuild that is true of. It refuses rather
/// than inventing one — see the `_required` pair in `ui/prompt.rs`.
///
/// Every answer goes through [`Remote::parse`], which is the same route
/// `riabuild remote <target>` takes, and it is what makes the three prompts
/// one question rather than three. A developer typing the address they know —
/// `ada@gpu:2222` — at a prompt labelled `Hostname` is not making a mistake,
/// and this used to build `ada@ada@gpu:2222` out of it by pasting the answers
/// together unread. Parsing the first answer also gives the two prompts after
/// it their defaults, so the parts already stated are offered back rather than
/// asked for again; and parsing the composed result validates the port and the
/// username too — the port used to be `unwrap_or(22)`, which silently turned a
/// typo into a connection to the wrong service.
fn ask_for_one(ctx: &Ctx, store: &Store) -> Result<Remote> {
    ctx.ui.heading("Adding a server");
    let whoami = store::whoami();
    let spec = Remote::parse(&ctx.ui.ask_required("Hostname  ", None)?, &whoami)?;
    let port = ctx
        .ui
        .ask_required("Port      ", Some(&spec.port.to_string()))?;
    let user = ctx.ui.ask_required("Username  ", Some(&spec.user))?;
    let mut remote = composed(&spec.host, &port, &user)?;
    remote.name = store::ask_name(&ctx.ui, &remote.host, &store.names());
    Ok(remote)
}

/// The three answers read back as one address, through the same
/// [`Remote::parse`] that `riabuild remote <target>` goes through.
///
/// Split from the prompts above because `ask_required` reads the real stdin —
/// it is the one prompt `Ui::scripted` cannot drive — so this is the half a
/// test can reach, and the half where every one of these bugs lived.
///
/// `default_user` is not a parameter: `host` and `user` have both already been
/// through `Remote::parse` by the time they get here, so there is nothing left
/// to default and passing one would only describe a case that cannot happen.
fn composed(host: &str, port: &str, user: &str) -> Result<Remote> {
    Remote::parse(&format!("{user}@{host}:{port}"), user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::record_for;
    use riabuild_runner::FakeRunner;

    fn remote(name: &str, host: &str) -> Remote {
        Remote {
            name: name.into(),
            host: host.into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// Two saved servers, the second of them the one most recently connected
    /// to — so "the default" and "the first one saved" are different answers
    /// and a test can tell which one was taken.
    fn two() -> Store {
        let mut store = Store::default();
        let mut older = record_for(&remote("build-01", "build-01.fly.dev"));
        older.last_used_at = riabuild_paths::config::now_secs().saturating_sub(5 * 86400);
        let mut newer = record_for(&remote("gpu", "gpu.internal"));
        newer.last_used_at = riabuild_paths::config::now_secs().saturating_sub(3600);
        store.remotes.push(older);
        store.remotes.push(newer);
        store
    }

    fn one() -> Store {
        let mut store = Store::default();
        store
            .remotes
            .push(record_for(&remote("build-01", "build-01.fly.dev")));
        store
    }

    #[test]
    fn a_number_in_range_is_the_server_at_that_position() {
        assert_eq!(parse_pick("1", 2), Some(Pick::Server(0)));
        assert_eq!(parse_pick("2", 2), Some(Pick::Server(1)));
        assert_eq!(parse_pick(" 2 \n", 2), Some(Pick::Server(1)));
    }

    #[test]
    fn the_number_after_the_last_server_adds_one() {
        assert_eq!(parse_pick("3", 2), Some(Pick::Add));
        assert_eq!(parse_pick("2", 1), Some(Pick::Add));
    }

    #[test]
    fn the_add_option_can_also_be_typed_as_a_word() {
        // What a developer types at a prompt whose last line reads "Add a
        // server", rather than counting the rows to find its number.
        assert_eq!(parse_pick("n", 2), Some(Pick::Add));
        assert_eq!(parse_pick("new", 2), Some(Pick::Add));
        assert_eq!(parse_pick("New", 2), Some(Pick::Add));
    }

    #[test]
    fn anything_that_is_not_an_answer_is_not_guessed_at() {
        // 0 and 4 both used to become server 1: the old picker parsed with
        // `unwrap_or(1)`, so a typo silently connected somewhere.
        assert_eq!(parse_pick("0", 2), None);
        assert_eq!(parse_pick("4", 2), None);
        assert_eq!(parse_pick("-1", 2), None);
        assert_eq!(parse_pick("", 2), None);
        assert_eq!(parse_pick("gpu", 2), None);
    }

    #[test]
    fn a_typed_number_settles_on_that_server() {
        assert_eq!(
            settle(&Ui::scripted(["1"]), &two().remotes, 1),
            Pick::Server(0)
        );
    }

    #[test]
    fn enter_settles_on_the_default() {
        // `ask` answers `None` for Enter, ^D, and nobody-there alike, and all
        // three mean "the one you offered".
        assert_eq!(
            settle(&Ui::scripted([""]), &two().remotes, 1),
            Pick::Server(1)
        );
        assert_eq!(
            settle(&Ui::scripted([] as [&str; 0]), &two().remotes, 1),
            Pick::Server(1)
        );
    }

    #[test]
    fn an_unusable_answer_is_asked_about_again() {
        let ui = Ui::scripted(["nonsense", "1"]);
        assert_eq!(settle(&ui, &two().remotes, 1), Pick::Server(0));
        assert_eq!(ui.asked().len(), 2, "{:?}", ui.asked());
    }

    #[test]
    fn a_developer_who_cannot_give_a_usable_answer_is_not_asked_forever() {
        let ui = Ui::scripted(["x", "y", "z", "1"]);
        assert_eq!(settle(&ui, &two().remotes, 1), Pick::Server(1));
        assert_eq!(ui.asked().len(), ATTEMPTS, "{:?}", ui.asked());
    }

    #[test]
    fn the_question_names_the_server_enter_would_take_not_only_its_number() {
        // `Ui::info` returns early under `--quiet` and `Ui::ask` does not, so
        // `riabuild --quiet remote` puts this question with the box above it
        // silently dropped. The number alone means nothing without that box;
        // the name is the one part the question can carry itself. Same reason
        // `accounts::command::confirm_question` names the account it is about.
        let ui = Ui::scripted(["1"]);
        settle(&ui, &two().remotes, 1);
        assert!(ui.asked()[0].contains("[2"), "{:?}", ui.asked());
        assert!(ui.asked()[0].contains("gpu"), "{:?}", ui.asked());
    }

    #[tokio::test]
    async fn one_saved_server_is_still_offered_a_choice_when_someone_is_there() {
        // The behaviour change: a single saved server used to reconnect
        // silently, so there was no way to add a second without spelling it
        // out on the command line.
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["1"]);
        let mut store = one();

        let chosen = pick(&mut ctx, &mut store).await.expect("connects");

        assert_eq!(chosen.name, "build-01");
        assert!(
            !ctx.ui.asked().is_empty(),
            "the developer has to be asked, or they cannot add a second server"
        );
    }

    #[tokio::test]
    async fn enter_takes_the_most_recently_used_server_not_the_first_one_saved() {
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.ui = Ui::scripted([""]);
        let mut store = two();

        let chosen = pick(&mut ctx, &mut store).await.expect("connects");

        assert_eq!(chosen.name, "gpu");
        // …and the bracket said so before Enter was pressed. Asserted in the
        // same test as the outcome on purpose: `settle` is handed a default
        // rather than working one out, so a `pick` that computed the bracket
        // from one server and connected to another would satisfy either
        // assertion alone. The developer's guarantee is that they agree.
        assert!(
            ctx.ui.asked()[0].contains("[2 · gpu]"),
            "{:?}",
            ctx.ui.asked()
        );
    }

    #[tokio::test]
    async fn a_run_with_no_terminal_and_one_saved_server_still_reconnects() {
        // Unchanged on purpose: this is `riabuild remote` in a script, and the
        // answer is not a guess when there is only one server it could mean.
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        let mut store = one();

        let chosen = pick(&mut ctx, &mut store).await.expect("reconnects");

        assert_eq!(chosen.name, "build-01");
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    #[tokio::test]
    async fn a_run_with_no_terminal_and_several_saved_servers_refuses_rather_than_guessing() {
        // Connecting is not a read: it provisions the server, mints it a
        // session, and lends it this laptop's GitHub sign-in. Taking a default
        // is the crate rule for a question riabuild is *offering*; this one has
        // to be answered or declined.
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        let mut store = two();

        let error = pick(&mut ctx, &mut store)
            .await
            .expect_err("must not pick a server nobody named");

        let failure = error
            .downcast_ref::<Failure>()
            .expect("must be the actionable Failure");
        assert!(
            failure.detail.contains("build-01") && failure.detail.contains("gpu"),
            "the developer has to be told which names they could pass: {}",
            failure.detail
        );
        assert!(
            failure.action.contains("riabuild remote"),
            "{}",
            failure.action
        );
    }

    /// The developer's own server, and one of the team's that this run's fetch
    /// described.
    fn mine_and_the_teams() -> Store {
        let mut store = Store::default();
        let mut mine = record_for(&remote("build-01", "build-01.fly.dev"));
        mine.last_used_at = riabuild_paths::config::now_secs().saturating_sub(5 * 86400);
        store.remotes.push(mine);
        let mut teams = crate::store::shared_record_for(&remote("gpu", "gpu.internal"), "k1");
        teams.last_used_at = riabuild_paths::config::now_secs().saturating_sub(3600);
        store.remotes.push(teams);
        store
    }

    #[tokio::test]
    async fn one_of_the_teams_servers_can_be_picked_by_its_number() {
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.ui = Ui::scripted(["2"]);
        let mut store = mine_and_the_teams();

        let chosen = pick(&mut ctx, &mut store).await.expect("connects");

        assert_eq!(chosen.name, "shared-gpu");
        assert_eq!(chosen.host, "gpu.internal");
        // The prefix travels on `Remote::name`, so the server's own shell
        // banner reads it back — and never on the address.
        assert_eq!(chosen.target(), "ada@gpu.internal");
    }

    #[tokio::test]
    async fn enter_can_take_one_of_the_teams_servers_too() {
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.ui = Ui::scripted([""]);
        let mut store = mine_and_the_teams();

        let chosen = pick(&mut ctx, &mut store).await.expect("connects");

        assert_eq!(chosen.name, "shared-gpu");
        assert!(
            ctx.ui.asked()[0].contains("[2 · shared-gpu]"),
            "{:?}",
            ctx.ui.asked()
        );
    }

    #[tokio::test]
    async fn a_server_the_leads_removed_is_not_in_the_box_and_is_not_the_default() {
        // Its address is a memory. It is still in the store — its session may
        // be live, and `remote list` shows it so it can be forgotten — but
        // nothing that leads to a connection may offer it.
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.ui = Ui::scripted([""]);
        let mut store = mine_and_the_teams();
        store.remotes[1].fresh = false;

        let chosen = pick(&mut ctx, &mut store).await.expect("connects");

        assert_eq!(chosen.name, "build-01");
        // The box is rendered from exactly this, so a server missing from it is
        // a server not on screen and not numbered.
        assert_eq!(
            store
                .reachable()
                .iter()
                .map(|record| record.display_name())
                .collect::<Vec<_>>(),
            vec!["build-01".to_string()]
        );
        assert!(
            ctx.ui.asked()[0].contains("[1 · build-01]"),
            "{:?}",
            ctx.ui.asked()
        );
    }

    #[tokio::test]
    async fn with_no_terminal_and_one_reachable_server_the_others_do_not_make_it_ambiguous() {
        // One of the team's servers went away; the developer's own is the only
        // thing left that can be connected to, so this is not a guess.
        let (mut ctx, _home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        let mut store = mine_and_the_teams();
        store.remotes[1].fresh = false;

        let chosen = pick(&mut ctx, &mut store).await.expect("reconnects");

        assert_eq!(chosen.name, "build-01");
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    // The Add answer is deliberately not driven through `pick` here. Acting on
    // it runs `ask_for_one`, whose questions go through `ask_required` — which
    // reads the real stdin rather than `Ui`'s scripted answers, so a test that
    // reached it would block on the terminal `cargo test` was launched from
    // rather than fail. `settle` is what makes that answer testable at all:
    // `the_add_option_can_also_be_typed_as_a_word` and
    // `the_number_after_the_last_server_adds_one` cover the decision, and what
    // is left in `pick` is the one match arm that calls the questions.
    //
    // What `composed` makes testable is the other half — what `ask_for_one`
    // does with the answers once it has them, which is where every bug in this
    // prompt has been.

    #[test]
    fn an_address_typed_at_the_hostname_prompt_is_read_as_one() {
        // A developer types the address they already know at the first prompt
        // it looks like it belongs in. The three answers used to be pasted
        // together unread, which made `ada@gpu:2222` into a connection to
        // `ada@ada@gpu:2222` — a host that does not exist, named in no error
        // message.
        let spec = Remote::parse("ada@gpu:2222", "root").expect("an address");
        assert_eq!(spec.host, "gpu");
        assert_eq!(spec.port, 2222);
        assert_eq!(spec.user, "ada");

        // …and pressing Enter at the two prompts after it takes those, which
        // is what the parsed answer is for.
        let remote = composed(&spec.host, &spec.port.to_string(), &spec.user).expect("a server");
        assert_eq!(remote.host, "gpu");
        assert_eq!(remote.port, 2222);
        assert_eq!(remote.user, "ada");
    }

    #[test]
    fn a_plain_hostname_still_takes_the_two_defaults() {
        let spec = Remote::parse("gpu.internal", "ada").expect("a hostname");
        let remote = composed(&spec.host, &spec.port.to_string(), &spec.user).expect("a server");
        assert_eq!(remote.host, "gpu.internal");
        assert_eq!(remote.port, 22);
        assert_eq!(remote.user, "ada");
    }

    #[test]
    fn an_answer_that_is_not_a_port_or_a_username_is_refused_rather_than_guessed_at() {
        // The port was `parse().unwrap_or(22)`, so a fat-fingered `2222x` was
        // silently a connection to 22 — the same class of guess
        // `anything_that_is_not_an_answer_is_not_guessed_at` covers for the
        // picker itself. Nothing at these prompts was validated at all: the
        // hostname prompt is where `-oProxyCommand=…` would have been typed.
        assert!(composed("gpu", "2222x", "ada").is_err());
        assert!(composed("gpu", "0", "ada").is_err());
        assert!(composed("-oProxyCommand=x", "22", "ada").is_err());
        assert!(composed("gpu", "22", "ada bob").is_err());
    }
}
