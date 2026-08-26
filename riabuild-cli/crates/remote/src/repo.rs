//! Which repository a remote run is about, asked on the laptop.
//!
//! It used to be asked on the server: `riabuild remote` runs the server's own
//! riabuild over `ssh -t`, that riabuild put the picker's question, and the
//! laptop added none. Which meant a developer answered "which server" here,
//! waited out a host key, a key check, an install and a session mint, and was
//! then asked "which repository" from the far side of a connection they had
//! already committed to — with the answer arriving too late to be worth
//! knowing before any of it.
//!
//! So both questions are put here, back to back, before the first `ssh`, and
//! the answer travels on as `--repo` — the flag that already existed for
//! naming one on the command line, and which the server's own picker already
//! stands aside for. Nothing about *who decides* moves: GitHub still answers
//! which repositories the developer may see, through the developer's own `gh`.
//! What moves is which machine's terminal the question is put at.
//!
//! Deciding is separated from asking the way `pick` separates them, and for
//! one more reason here: the laptop must write nothing about itself.
//! `repo::pick::choose` records what it settled on in this machine's
//! `config.json`, which is the right thing for a run that is about this
//! machine and exactly wrong for one that is about a server —
//! `riabuild remote gpu` would leave the laptop working on whatever the server
//! was told to. `repo::pick::offer` is the same question with none of that,
//! and where the answer belongs instead is `remotes.json`, beside the server
//! it was chosen for.

use crate::{Remote, Request, store::Store};
use riabuild_api::Repo;
use riabuild_tasks::Ctx;
use riabuild_tasks::repo::pick::{Offer, offer};
use std::collections::BTreeMap;

/// The repository to hand the server, or `None` to leave the question to it.
///
/// `None` is never a guess riabuild made quietly — it is the two cases where
/// this laptop has nothing to say: a run with nobody at the terminal on a
/// server that has never been set up from here, and a `--check` that must
/// change nothing on either machine.
pub async fn choose_for(
    ctx: &Ctx,
    request: &Request,
    store: &Store,
    remote: &Remote,
) -> Option<String> {
    // Named on the command line, and `riabuild remote --repo payments build-01`
    // is a developer who has already answered. Asking anyway would be asking a
    // question whose answer is in the argv it was typed in.
    if let Some(named) = &request.repo {
        return Some(named.clone());
    }

    // What this laptop last set *this server* up for — not what this laptop
    // itself is working on. The two are routinely different: a developer whose
    // laptop is on `ai-builders-hub` may keep `payments` on the GPU box, and
    // before this question moved here that memory lived on the server, where
    // its own picker offered it. Passing `--repo` unconditionally is what takes
    // that away, so the memory has to come with it.
    let remembered = store
        .find(&remote.name)
        .map(|record| record.repo.as_str())
        .filter(|slug| !slug.is_empty())
        .and_then(|slug| Repo::parse(slug).ok());

    // `--check` reports and changes nothing, on both machines: the server is
    // run with `--check --no-shell` and nothing else, so there is nothing for
    // an answer to reach. A question is a poor thing to put to somebody who
    // asked for a report — `repo::pick`'s own rule, and `provision`'s.
    //
    // With no terminal the remembered answer still travels, because it is not a
    // guess: it is what this laptop set that same server up for last time.
    if request.check || !ctx.ui.interactive() {
        return remembered.map(|repo| repo.slug().to_string());
    }

    // A laptop with no org configuration cannot draw the box — it has no owner
    // to list, and no default to offer. That is a machine `flow::run` has
    // already stopped, so what is left here is a caller that never connected;
    // it leaves the question where it used to be rather than inventing one.
    let Some(org_default) = ctx.org.as_ref().and_then(|org| org.default_repo().ok()) else {
        return remembered.map(|repo| repo.slug().to_string());
    };

    // What Enter takes, in the order the developer would expect: this server's
    // last repository, then the one this laptop is working on, then the org
    // default. `Ctx::repo` is the last two.
    let default = match remembered {
        Some(repo) => repo,
        None => ctx.repo().unwrap_or_else(|_| org_default.clone()),
    };

    // Empty, and deliberately so: `known` marks the rows a machine already has
    // a checkout of, and this laptop's checkouts are not the server's. Marking
    // them would be printing a guess about a filesystem nothing here has looked
    // at — and the one row it would usefully mark, the repository this server
    // was last set up for, is already the default and already first.
    let nothing_cloned = BTreeMap::new();
    let chosen = offer(
        ctx,
        Offer {
            default: &default,
            org_default: &org_default,
            known: &nothing_cloned,
            on: Some(&remote.name),
        },
    )
    .await;
    Some(chosen.slug().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, record_for};
    use riabuild_runner::FakeRunner;
    use riabuild_ui::Ui;

    fn remote() -> Remote {
        Remote {
            name: "build-01".into(),
            host: "build-01.fly.dev".into(),
            port: 22,
            user: "ada".into(),
        }
    }

    /// A store holding one server, set up last time for `slug`.
    fn store_for(slug: &str) -> Store {
        let mut store = Store::default();
        let mut record = record_for(&remote());
        record.repo = slug.to_string();
        store.remotes.push(record);
        store
    }

    async fn ctx() -> (Ctx, tempfile::TempDir) {
        let (mut ctx, home) = riabuild_tasks::testing::ctx_with(FakeRunner::new()).await;
        ctx.org = Some(riabuild_tasks::testing::org_config());
        (ctx, home)
    }

    #[tokio::test]
    async fn a_repository_named_on_the_command_line_is_not_asked_about_again() {
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted(["2"]);
        let request = Request {
            repo: Some("Clubria/payments".to_string()),
            ..Request::default()
        };

        let chosen = choose_for(&ctx, &request, &Store::default(), &remote()).await;

        assert_eq!(chosen.as_deref(), Some("Clubria/payments"));
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    #[tokio::test]
    async fn enter_takes_the_repository_this_laptop_last_set_that_server_up_for() {
        // The memory that used to live on the server. Asserted through the
        // bracket as well as the answer, because the question is what the
        // developer agrees to by pressing Enter.
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted([""]);

        let chosen = choose_for(
            &ctx,
            &Request::default(),
            &store_for("Clubria/payments"),
            &remote(),
        )
        .await;

        assert_eq!(chosen.as_deref(), Some("Clubria/payments"));
        assert!(
            ctx.ui.asked()[0].contains("Clubria/payments"),
            "{:?}",
            ctx.ui.asked()
        );
    }

    #[tokio::test]
    async fn the_question_names_the_server_it_is_being_asked_about() {
        // Both questions are now put at the same terminal, one after the other,
        // and an unqualified "which repository?" there reads as one about the
        // laptop in front of the developer.
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted([""]);

        choose_for(&ctx, &Request::default(), &Store::default(), &remote()).await;

        assert!(
            ctx.ui.asked()[0].contains("on build-01"),
            "{:?}",
            ctx.ui.asked()
        );
    }

    #[tokio::test]
    async fn a_server_this_laptop_has_never_set_up_is_offered_what_the_laptop_works_on() {
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted([""]);
        ctx.repo = Some(Repo::parse("Clubria/ledger").expect("parses"));

        let chosen = choose_for(&ctx, &Request::default(), &Store::default(), &remote()).await;

        assert_eq!(chosen.as_deref(), Some("Clubria/ledger"));
    }

    #[tokio::test]
    async fn a_typed_name_is_taken_whether_or_not_github_listed_it() {
        // `gh` is not installed in this `Ctx`, so the box has one row. Typing a
        // name still works — that is `settle`'s rule, and it is the whole
        // reason the ten-row cut is safe.
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted(["payments"]);

        let chosen = choose_for(&ctx, &Request::default(), &Store::default(), &remote()).await;

        assert_eq!(chosen.as_deref(), Some("Clubria/payments"));
    }

    #[tokio::test]
    async fn a_check_asks_nothing_and_sends_nothing() {
        // The server is run with `--check --no-shell` and nothing else, so an
        // answer would reach nothing — and `--check` must leave both machines
        // as it found them.
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted([""]);
        let request = Request {
            check: true,
            ..Request::default()
        };

        let chosen = choose_for(&ctx, &request, &Store::default(), &remote()).await;

        assert_eq!(chosen, None);
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    #[tokio::test]
    async fn with_no_terminal_the_servers_own_last_repository_still_travels() {
        // Not a guess: it is what this laptop set this server up for last time,
        // and the alternative is an unattended run silently moving the server
        // onto the org default.
        let (ctx, _home) = ctx().await;

        let chosen = choose_for(
            &ctx,
            &Request::default(),
            &store_for("Clubria/payments"),
            &remote(),
        )
        .await;

        assert_eq!(chosen.as_deref(), Some("Clubria/payments"));
        assert!(ctx.ui.asked().is_empty(), "{:?}", ctx.ui.asked());
    }

    #[tokio::test]
    async fn with_no_terminal_and_nothing_remembered_the_question_is_left_to_the_server() {
        let (ctx, _home) = ctx().await;

        let chosen = choose_for(&ctx, &Request::default(), &Store::default(), &remote()).await;

        assert_eq!(chosen, None);
    }

    #[tokio::test]
    async fn the_laptops_own_repository_is_not_changed_by_answering_for_a_server() {
        // `repo::pick::choose` writes `config.json`; this must not. A developer
        // who sets `gpu` up for `payments` has not said anything about the
        // machine they are sitting at.
        let (mut ctx, _home) = ctx().await;
        ctx.ui = Ui::scripted(["payments"]);

        choose_for(&ctx, &Request::default(), &Store::default(), &remote()).await;

        assert_eq!(ctx.config.active_repo, None);
        assert_eq!(ctx.repo, None);
        let reloaded = store::Store::load(ctx.paths.as_ref()).await;
        assert!(reloaded.remotes.is_empty(), "nothing is persisted here");
    }
}
