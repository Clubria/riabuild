//! Which of this machine's checkouts a command that acts on one is about.
//!
//! A different question from the provisioning picker next door: it is asked
//! of the repositories this machine has actually cloned, because a repository
//! with no checkout has nothing to move.

use super::super::render::{self, Row};
use super::answer::{ATTEMPTS, Answer, settle};
use super::now;
use crate::Ctx;
use anyhow::Result;
use riabuild_api::Repo;
use riabuild_ui::Ui;

/// Which of this machine's checkouts a command that acts on one is about.
///
/// `None` is "there is nothing to choose": no checkout recorded at all, which on
/// a machine whose migration has not run still means the one path `project_dir`
/// finds. The caller keeps its own behaviour for that case rather than being
/// handed a repository riabuild invented.
///
/// Restricted to repositories this machine has actually cloned, because a
/// repository with no checkout has nothing to move. With exactly one there is no
/// question to put, which is every machine that has only ever worked on one
/// repository — so `riabuild move-project ~/elsewhere` there is unchanged.
pub async fn choose_cloned(ctx: &mut Ctx) -> Result<Option<Repo>> {
    let mut cloned: Vec<Repo> = ctx
        .config
        .repos
        .keys()
        .filter_map(|slug| Repo::parse(slug).ok())
        .collect();
    if cloned.is_empty() {
        return Ok(None);
    }

    // What Enter takes: the repository this machine is working on, if it is one
    // of the cloned ones.
    let active = ctx
        .config
        .active_repo
        .as_deref()
        .and_then(|slug| Repo::parse(slug).ok())
        .filter(|repo| cloned.contains(repo));
    let default = active.clone().unwrap_or_else(|| cloned[0].clone());
    cloned.sort_by_key(|repo| (*repo != default, repo.slug().to_string()));

    if cloned.len() == 1 {
        let only = cloned.remove(0);
        ctx.repo = Some(only.clone());
        return Ok(Some(only));
    }

    let chosen = if ctx.ui.interactive() {
        let org_default = ctx.org.as_ref().and_then(|org| org.default_repo().ok());
        let rows: Vec<Row> = cloned
            .iter()
            .map(|repo| Row {
                default: org_default.as_ref() == Some(repo),
                repo: repo.clone(),
                pushed_at: 0,
                cloned: true,
                // This box is drawn from `config.json` alone — no listing, and
                // so nothing that could describe a repository. `riabuild
                // move-project` is a question about directories on this
                // machine, and spending a GitHub round trip on prose for it
                // would be the picker's cost paid by a command that does not
                // need the answer.
                description: String::new(),
            })
            .collect();
        ctx.ui.info("");
        ctx.ui.info(&render::repos_box(
            "Repositories checked out on this machine",
            &rows,
            0,
            now(),
            ctx.ui.theme(),
        ));
        ask_cloned(&ctx.ui, &cloned, &default)
    } else {
        // The crate rule: a prompt offers a choice, so nobody there takes the
        // default. `move-project` with no path then fails at its own question,
        // which is the failure that was always there.
        default
    };

    ctx.repo = Some(chosen.clone());
    Ok(Some(chosen))
}

/// The question, for a box whose rows are all checkouts.
///
/// A typed name has to be one of them: a repository this machine has not cloned
/// has no directory to move, and "which repository" is a better place to say so
/// than a failed `rename`.
fn ask_cloned(ui: &Ui, cloned: &[Repo], default: &Repo) -> Repo {
    let question = format!("Which repository's checkout? (press enter for {default})");
    for _ in 0..ATTEMPTS {
        let Some(answer) = ui.ask(&question) else {
            break;
        };
        match settle(&answer, cloned.len(), default.owner()) {
            Ok(Answer::Default) => break,
            Ok(Answer::Listed(index)) => return cloned[index].clone(),
            Ok(Answer::Named(named)) => match cloned.contains(&named) {
                true => return named,
                false => ui.warn(&format!(
                    "riabuild has no checkout of {named} on this machine — \
                     run `riabuild` and pick it to clone one"
                )),
            },
            Err(objection) => ui.warn(&objection),
        }
    }
    default.clone()
}
