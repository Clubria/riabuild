//! Where a checkout goes, and why a typed path might not be allowed to be
//! that place.
//!
//! The prompt offers riabuild's own answer and takes Enter for it, so a
//! developer with no opinion still decides nothing. The two refusals below it
//! exist because the prompt runs on a server too, where several developers
//! share one Unix account and are kept apart only by which directories belong
//! to whom.

use crate::Ctx;
use riabuild_paths::{contract_tilde, expand_tilde};
use std::path::{Path, PathBuf};

/// How many times riabuild will re-ask before falling back to its own answer.
const ATTEMPTS: usize = 3;

/// Where the checkout should go, offering riabuild's answer and letting the
/// developer say otherwise.
///
/// The default is what riabuild would have done silently before, so a developer
/// with no opinion still makes no decision: Enter, no terminal, or an answer
/// riabuild cannot use all land on it.
///
/// `default` is `Ctx::default_checkout`'s answer, never
/// `paths::default_project_dir`'s. The two differ on a server, where several
/// developers share one Unix account and the platform default is one directory
/// all of them would land in — so offering it here would put one working tree,
/// one set of branches, and one `.env.local` of brokered secrets in front of
/// everybody who pressed Enter.
pub(super) async fn choose_dir(ctx: &Ctx, home: &Path, default: PathBuf) -> PathBuf {
    let question = format!(
        "The repository will be installed at {}. Choose a different path? (press enter for default)",
        contract_tilde(&default, home)
    );

    // Bounded, because a developer who cannot give a usable path is better
    // served by riabuild picking one than by being asked forever.
    for _ in 0..ATTEMPTS {
        let Some(answer) = ctx.ui.ask(&question) else {
            break;
        };
        let chosen = expand_tilde(&answer, home);
        match objection(ctx, &chosen, &default).await {
            None => return chosen,
            Some(objection) => ctx.ui.warn(&objection),
        }
    }

    ctx.ui.note(&format!(
        "Using {} for the checkout",
        contract_tilde(&default, home)
    ));
    default
}

/// Why riabuild cannot clone into a path the developer typed, if it cannot.
///
/// Checked while they are still being asked. The alternative is learning it
/// from a failed `gh repo clone` several seconds later, by which point the
/// answer has been recorded and the developer has to work out how to change it.
async fn objection(ctx: &Ctx, path: &Path, default: &Path) -> Option<String> {
    if !path.is_absolute() {
        return Some(format!(
            "{} is relative — give a path starting with / or ~/",
            path.display()
        ));
    }

    if let Some(escape) = outside_the_namespace(ctx, path, default) {
        return Some(escape);
    }

    // A checkout already sitting there is not an obstacle: `apply` adopts one,
    // and adopting the checkout a developer already has is half the reason to
    // offer the choice at all.
    if tokio::fs::try_exists(path.join(".git"))
        .await
        .unwrap_or(false)
    {
        return None;
    }

    let Ok(mut reader) = tokio::fs::read_dir(path).await else {
        // Nothing there yet, which is the ordinary case.
        return None;
    };
    match reader.next_entry().await {
        Ok(Some(_)) => Some(format!(
            "{} already has files in it — git will not clone into it",
            path.display()
        )),
        _ => None,
    }
}

/// Why a typed path is not somewhere this developer may put a checkout, if it
/// is not. Always `None` on a laptop, where the whole filesystem is theirs.
///
/// On a server it is not. Several developers share one Unix account and are kept
/// apart only by which directories belong to whom: state under
/// `paths::remote_namespace`, checkouts under their own directory in
/// `paths::remote_project_dir`. The prompt above runs against a real terminal
/// there — `riabuild remote` connects with `ssh -t` — so without this an
/// absolute path typed at it walks straight out of the namespace, and the
/// developer ends up in a colleague's tree: one working tree, one set of
/// branches, and one `.env.local` holding the brokered Infisical secrets of
/// whoever ran riabuild last.
fn outside_the_namespace(ctx: &Ctx, path: &Path, default: &Path) -> Option<String> {
    // Laptops are untouched by this: there is no co-tenant to collide with, and
    // where a developer keeps their own checkout is their business. `?` rather
    // than an `if`, because clippy reads the explicit form as a `?` waiting to
    // happen — the meaning is "not a server, no objection".
    ctx.server.as_ref()?;
    let home = ctx.paths.home();

    // The developer's own directory under the org folder. Taken from the
    // *parent* of the default rather than rebuilt, so somebody whose GitHub
    // login was already claimed — `Ctx::default_checkout` hands them `<login>-2`
    // — is allowed their own directory and not the first one.
    let own = default.parent().unwrap_or(default);
    // `Path::starts_with` compares whole components, so `~/Clubria/ada-2` is not
    // inside `~/Clubria/ada`. A string prefix test would have said it was.
    if path.starts_with(own) {
        return None;
    }
    // The state namespace is this developer's alone too. An odd place for a
    // checkout, but not a shared one, so there is nothing here to refuse.
    if let Some(member) = ctx.member.as_ref()
        && path.starts_with(riabuild_paths::remote_namespace(&home, &member.member_id))
    {
        return None;
    }

    Some(format!(
        "{} is not yours on this server — several developers share this account, \
         so the checkout has to sit under {}",
        path.display(),
        contract_tilde(own, &home)
    ))
}
