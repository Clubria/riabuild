//! `riabuild internal infisical` — the developer's own `infisical`, signed in
//! for the length of one command.
//!
//! What `~/.riabuild/bin/infisical` runs, so this is what happens when anybody
//! in a riabuild shell types `infisical`. It closes the gap the brokering
//! design left behind: `env_local` filled `.env.dev` with a credential minted
//! for that pull and thrown away, and the developer who then ran `infisical
//! export` themselves met a CLI nobody had ever logged in. The fix cannot be
//! `infisical login`, which writes a credential to the machine. It is this —
//! mint one for *this* command, put it in the child's environment, and let it
//! expire — which is what `env_local` has always done, now reachable by hand.
//!
//! Design: `../../../../../docs/superpowers/specs/2026-08-27-infisical-session-login-design.md`.

use anyhow::Result;
use riabuild_api::secrets::BrokeredToken;
use riabuild_runner::RunOptions;
use riabuild_tasks::Ctx;

/// Runs infisical with a credential brokered for this one command.
///
/// **Nothing is printed on stdout, ever.** `infisical export > .env` is an
/// ordinary thing to type, and the child inherits this process's stdout: a
/// riabuild banner would land in the developer's file. Everything riabuild has
/// to say goes to stderr, which is also where infisical writes its own
/// diagnostics.
///
/// A failure to broker is **not** fatal, for the reason it is not fatal in the
/// ngrok shim: `infisical --version` and `infisical scan` are worth having on a
/// plane. riabuild says why on stderr and hands the terminal over anyway, so
/// what the developer meets is infisical's own "you must be logged in" with an
/// explanation above it rather than instead of it.
pub(crate) async fn run(ctx: &mut Ctx, args: &[String]) -> Result<i32> {
    let binary = ctx.infisical();
    if !tokio::fs::try_exists(&binary).await.unwrap_or(false) {
        // The shim is written by the task that installs the binary, so this is
        // a half-removed `~/.riabuild` rather than a machine mid-setup. Said
        // plainly, because the alternative is `No such file or directory`
        // naming a path the developer never chose.
        return Err(riabuild_ui::Failure::new(
            "running infisical",
            "Run `riabuild` to finish setting this machine up, then try again.",
        )
        .detail(format!("riabuild has not installed infisical at {binary}"))
        .into());
    }

    let brokered = if needs_credentials(args) {
        credential_for_one_command(ctx).await
    } else {
        None
    };

    let options = RunOptions {
        env: session_env(brokered.as_ref(), &set_by_the_developer),
        // The one place `cwd: None` means *inherit*, and the one place that is
        // right: `run_interactive` is a handoff, and infisical reads
        // `.infisical.json` from the directory it was started in. A developer
        // typing `infisical` in their checkout must get the same answer they
        // would from the binary directly.
        ..Default::default()
    };
    let args = scoped(args, brokered.as_ref());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    ctx.runner
        .run_interactive(&binary, &borrowed, &options)
        .await
}

/// A credential for this one command, or `None` and a line saying why.
///
/// Every invocation brokers its own, which is the same trade ngrok's shim
/// makes: a credential a lead revokes this morning stops working this morning,
/// and the audit row says somebody read the team's secrets rather than that
/// somebody opened a terminal.
async fn credential_for_one_command(ctx: &mut Ctx) -> Option<BrokeredToken> {
    if let Err(error) = ctx.connect().await {
        ctx.ui.warn(&format!(
            "riabuild could not reach riabuild.clubria.com, so infisical is running signed out ({error})"
        ));
        return None;
    }
    if ctx.org.is_none() {
        ctx.ui.warn(
            "This machine is not signed in to riabuild, so infisical is running signed out. \
             Run `riabuild` to sign in.",
        );
        return None;
    }
    match riabuild_api::secrets::broker(&ctx.api).await {
        Ok(brokered) => Some(brokered),
        Err(error) => {
            ctx.ui.warn(&format!(
                "riabuild could not broker an Infisical credential, so infisical is running signed out ({error})"
            ));
            None
        }
    }
}

/// The infisical subcommands that have nothing to do with the team's project.
///
/// Skipping the broker for these is a saving rather than a rule: `infisical
/// scan` is the one a developer installs as a pre-commit hook, so it runs on
/// every commit, and minting a credential and writing an audit row to scan a
/// diff for leaked secrets answers a question nobody asked. The others are the
/// developer's own machine — a login they are entitled to make for themselves,
/// the local credential store, and the help.
const WITHOUT_A_CREDENTIAL: &[&str] = &[
    "completion",
    "help",
    "login",
    "reset",
    "scan",
    "user",
    "vault",
];

/// Whether this invocation is worth brokering a credential for.
///
/// Conservative in one direction on purpose: anything this does not recognise
/// gets a credential, so a subcommand infisical adds after this was written is
/// signed in rather than mysteriously signed out. The scan is naive about flags
/// that take a value — `infisical -l trace scan` reads `trace` as the
/// subcommand and brokers — and that is the harmless direction of the same
/// choice.
///
/// Arguments after a bare `--` belong to whatever `infisical run` is starting,
/// not to infisical, so they are not read at all. Without that, `infisical run
/// -- ./deploy --help` would decide it was a help invocation and hand the
/// developer's server no secrets.
fn needs_credentials(args: &[String]) -> bool {
    let infisicals = infisicals_own(args).0;
    if infisicals.is_empty() {
        // A bare `infisical` prints its own help.
        return false;
    }
    if infisicals
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help" | "-v" | "--version"))
    {
        return false;
    }
    let Some(subcommand) = subcommand(infisicals) else {
        return false;
    };
    !WITHOUT_A_CREDENTIAL.contains(&subcommand)
}

/// The subcommands riabuild answers "which secrets?" for.
///
/// Every one of them takes `--env` and `--path` — checked against infisical
/// 0.43.120, including the whole `secrets` tree, where `--env` is a group-wide
/// flag and `folders get` spells the other one `-p, --path`. Nothing else is
/// listed, and that is the safe direction here rather than the timid one:
/// `--path` on a subcommand that has none is not a flag infisical ignores, it
/// is `unknown flag` and a command that used to work.
const SCOPED_BY_THE_TEAM: &[&str] = &["export", "run", "secrets"];

/// The developer's command, with the team's environment and secret path filled
/// in where they named neither.
///
/// **This is the half that environment variables cannot do**, and finding out
/// why cost a wrong-looking success: `INFISICAL_ENVIRONMENT` and
/// `INFISICAL_SECRET_PATH` are read only by the commands whose own flag carries
/// no default, and `export`, `run` and `secrets` all default `--env` to `dev`
/// and `--path` to `/`. A default counts as an answer, so those two variables
/// are inert exactly where a developer needs them. On a team whose secrets live
/// in a folder — which is what `INFISICAL_SECRET_PATH` on the riabuild-web
/// deployment means, and what `env_local` passes as `--path` on every pull —
/// the result was an `infisical export` that authenticated perfectly, exited 0,
/// and printed nothing. An empty answer that looks like a working command is
/// worse than the sign-in error this whole change removes.
///
/// So the scope is passed the way `env_local` passes it, and the shape is the
/// one the Codex and Grok launchers already use for `--yolo`: riabuild supplies
/// a default and **stands aside wherever the developer expressed one**. Neither
/// value is a secret — both are already in `env_local`'s argv — so there is no
/// `ps` question here.
///
/// Nothing is added after a bare `--`. What follows it belongs to the program
/// `infisical run` is starting, and a `--path` appended there would be that
/// program's argument rather than infisical's.
fn scoped(args: &[String], brokered: Option<&BrokeredToken>) -> Vec<String> {
    let Some(brokered) = brokered else {
        return args.to_vec();
    };
    let (infisicals, rest) = infisicals_own(args);
    let Some(subcommand) = subcommand(infisicals) else {
        return args.to_vec();
    };
    if !SCOPED_BY_THE_TEAM.contains(&subcommand) {
        return args.to_vec();
    }

    let mut scoped = infisicals.to_vec();
    if !brokered.environment.is_empty() && !named(infisicals, "--env", "-e") {
        scoped.push(format!("--env={}", brokered.environment));
    }
    if !brokered.secret_path.is_empty() && !named(infisicals, "--path", "-p") {
        scoped.push(format!("--path={}", brokered.secret_path));
    }
    scoped.extend_from_slice(rest);
    scoped
}

/// Splits the developer's arguments at the first bare `--`, keeping it with the
/// far side.
///
/// Everything past it belongs to whatever `infisical run` is starting, and is
/// neither read nor added to.
fn infisicals_own(args: &[String]) -> (&[String], &[String]) {
    match args.iter().position(|arg| arg == "--") {
        Some(at) => args.split_at(at),
        None => (args, &[]),
    }
}

/// The first word that is not a flag — infisical's subcommand, near enough.
fn subcommand(infisicals: &[String]) -> Option<&str> {
    infisicals
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(String::as_str)
}

/// Whether the developer named this option themselves, in any of its spellings.
///
/// `--path /apps`, `--path=/apps` and `-p /apps` all count. Getting this wrong
/// in the direction of "they did not" is what would pass the option twice —
/// resolved to the last one for `--env`, and to *both* for `run --path`, which
/// is a `stringArray`. Either way riabuild would be answering a question the
/// developer had already answered.
fn named(infisicals: &[String], long: &str, short: &str) -> bool {
    let assigned = format!("{long}=");
    infisicals
        .iter()
        .any(|arg| arg == long || arg == short || arg.starts_with(&assigned))
}

/// Whether the developer has set this variable themselves.
///
/// Empty counts as unset: `INFISICAL_TOKEN=` reads to infisical as *not
/// authenticated*, so treating it as an answer would leave the one shell that
/// exported it permanently unable to reach the team's secrets.
fn set_by_the_developer(key: &str) -> bool {
    std::env::var(key).is_ok_and(|value| !value.is_empty())
}

/// What the child gets on top of the environment it inherits.
///
/// **riabuild fills in what the developer has not.** Each of these is a value
/// somebody may legitimately have chosen for themselves — a second Infisical
/// project, a self-hosted instance, an environment other than the one their
/// `.env` files come from — and a shim that overwrote them would make those
/// choices impossible to express anywhere riabuild's `PATH` reaches, which is
/// the whole shell. A developer who has set `INFISICAL_TOKEN` is not brokered
/// for at all; see [`run`] above.
///
/// The credential reaches infisical **here and nowhere else**: not in an
/// argument, where `ps` would show it to every account on the machine, and not
/// in a file.
///
/// `INFISICAL_ENVIRONMENT` and `INFISICAL_SECRET_PATH` are here for the
/// commands that read them — the agent and proxy ones, whose own flags carry no
/// default. They are **not** what scopes `export`, `run` and `secrets`, whose
/// `--env` and `--path` default to `dev` and `/` and therefore never consult
/// them; [`scoped`] is what answers for those, and the two are not
/// interchangeable.
fn session_env(
    brokered: Option<&BrokeredToken>,
    already_set: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    // Not a credential, and set whether or not one was brokered: infisical's
    // update notice tells the developer to `sudo apt-get install infisical`,
    // which would put a second, unverified copy on the machine in front of the
    // one riabuild owns and checks.
    let mut wanted = vec![("INFISICAL_DISABLE_UPDATE_CHECK", "true".to_string())];
    if let Some(brokered) = brokered {
        wanted.push(("INFISICAL_TOKEN", brokered.token.clone()));
        if !brokered.site_url.is_empty() {
            // Guarded, because the field is a `default` and a deployment that
            // sends none would otherwise point infisical at `/api`.
            wanted.push(("INFISICAL_API_URL", format!("{}/api", brokered.site_url)));
        }
        wanted.push(("INFISICAL_PROJECT_ID", brokered.project_id.clone()));
        wanted.push(("INFISICAL_ENVIRONMENT", brokered.environment.clone()));
        wanted.push(("INFISICAL_SECRET_PATH", brokered.secret_path.clone()));
    }
    wanted
        .into_iter()
        .filter(|(key, value)| !value.is_empty() && !already_set(key))
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What riabuild-web hands back for one command.
    fn brokered() -> BrokeredToken {
        BrokeredToken {
            token: "st.brokered".into(),
            expires_at: 1,
            project_id: "p1".into(),
            environment: "dev".into(),
            environments: vec!["dev".into(), "staging".into()],
            // The shim fills in the *primary* folder — the last one the
            // deployment named — and never the whole list: a command the
            // developer typed is scoped, not assembled. Merging several
            // folders belongs to `env_local`, which writes a file that has to
            // be complete.
            secret_path: "/apps".into(),
            secret_paths: vec!["/apps/frontend".into(), "/apps".into()],
            site_url: "https://app.infisical.com".into(),
            secrets_updated_at: 0,
        }
    }

    fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn args(typed: &[&str]) -> Vec<String> {
        typed.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn the_credential_reaches_infisical_in_its_environment() {
        // The whole feature: after this, `infisical export` in a riabuild shell
        // is the same command riabuild runs for `.env.dev`, with the same
        // credential, minted for this one invocation.
        let env = session_env(Some(&brokered()), &|_| false);
        assert_eq!(value(&env, "INFISICAL_TOKEN"), Some("st.brokered"));
        assert_eq!(value(&env, "INFISICAL_PROJECT_ID"), Some("p1"));
        assert_eq!(
            value(&env, "INFISICAL_API_URL"),
            Some("https://app.infisical.com/api")
        );
        assert_eq!(value(&env, "INFISICAL_ENVIRONMENT"), Some("dev"));
        assert_eq!(value(&env, "INFISICAL_SECRET_PATH"), Some("/apps"));
    }

    #[test]
    fn a_value_the_developer_chose_is_left_alone() {
        // `~/.riabuild/bin` leads `PATH` in the environment shell, so this shim
        // is every `infisical` the developer can reach. One that overwrote
        // these would make "work against staging for an hour" impossible to
        // express anywhere.
        let env = session_env(Some(&brokered()), &|key| key == "INFISICAL_ENVIRONMENT");
        assert_eq!(value(&env, "INFISICAL_ENVIRONMENT"), None, "{env:?}");
        assert_eq!(value(&env, "INFISICAL_TOKEN"), Some("st.brokered"));
    }

    #[test]
    fn a_deployment_that_sent_no_site_url_is_not_turned_into_a_path() {
        // `siteUrl` is a `default`, and the obvious `format!("{site_url}/api")`
        // sends infisical to `/api` — a relative path, so a request that fails
        // in a way that reads as the credential being wrong.
        let mut brokered = brokered();
        brokered.site_url = String::new();
        let env = session_env(Some(&brokered), &|_| false);
        assert_eq!(value(&env, "INFISICAL_API_URL"), None, "{env:?}");
    }

    #[test]
    fn an_invocation_with_no_credential_still_silences_the_update_notice() {
        // infisical's own notice says to `sudo apt-get install infisical`,
        // which puts an unverified second copy in front of riabuild's — so it
        // is suppressed on every invocation, signed in or not.
        let env = session_env(None, &|_| false);
        assert_eq!(
            value(&env, "INFISICAL_DISABLE_UPDATE_CHECK"),
            Some("true"),
            "{env:?}"
        );
        assert_eq!(env.len(), 1, "{env:?}");
    }

    #[test]
    fn a_command_that_reads_the_teams_secrets_is_brokered_for() {
        for typed in [
            vec!["export", "--env=dev"],
            vec!["run", "--", "pnpm", "dev"],
            vec!["secrets"],
            vec!["init"],
            // Not recognised, so signed in rather than mysteriously not.
            vec!["something-infisical-added-later"],
        ] {
            assert!(needs_credentials(&args(&typed)), "{typed:?}");
        }
    }

    #[test]
    fn a_command_with_nothing_to_do_with_the_team_is_not() {
        for typed in [
            vec![],
            vec!["--version"],
            vec!["-h"],
            vec!["export", "--help"],
            // The one that would otherwise mint a credential and write an
            // audit row on every commit.
            vec!["scan", "git-changes"],
            vec!["login"],
        ] {
            assert!(!needs_credentials(&args(&typed)), "{typed:?}");
        }
    }

    #[test]
    fn what_infisical_run_starts_is_not_read_as_infisicals_own_arguments() {
        // `infisical run -- ./deploy --help` is a deploy that wants secrets.
        // Reading past the `--` sees `--help` and hands it none, which presents
        // as a program that ran with an empty environment for no stated reason.
        assert!(needs_credentials(&args(&[
            "run", "--", "./deploy", "--help"
        ])));
    }

    #[test]
    fn the_teams_environment_and_folder_are_filled_in() {
        // The bug this exists for: with only the environment variables set,
        // `infisical export` authenticated, exited 0 and printed nothing,
        // because `--env` and `--path` carry defaults and a default counts as
        // an answer. A team whose secrets live in a folder got silence.
        let scoped = scoped(&args(&["export"]), Some(&brokered()));
        assert_eq!(scoped, args(&["export", "--env=dev", "--path=/apps"]));
    }

    #[test]
    fn an_environment_the_developer_asked_for_is_not_overruled() {
        // `~/.riabuild/bin` leads `PATH`, so this shim is the only infisical
        // they can reach. Appending the team's answer beside theirs resolves to
        // whichever infisical reads last, which is riabuild quietly winning.
        assert_eq!(
            scoped(&args(&["export", "--env", "staging"]), Some(&brokered())),
            args(&["export", "--env", "staging", "--path=/apps"])
        );
        // And in the other spellings.
        for typed in [
            vec!["export", "--env=staging"],
            vec!["run", "-e", "staging"],
        ] {
            let out = scoped(&args(&typed), Some(&brokered()));
            assert!(!out.iter().any(|arg| arg == "--env=dev"), "{out:?}");
        }
    }

    #[test]
    fn a_folder_the_developer_asked_for_is_not_overruled() {
        for typed in [
            vec!["secrets", "--path", "/other"],
            vec!["secrets", "--path=/other"],
            // `secrets folders get` spells it `-p`.
            vec!["secrets", "folders", "get", "-p", "/other"],
        ] {
            let scoped = scoped(&args(&typed), Some(&brokered()));
            assert!(
                !scoped.iter().any(|arg| arg == "--path=/apps"),
                "{scoped:?}"
            );
        }
    }

    #[test]
    fn a_subcommand_that_takes_neither_flag_is_left_exactly_as_typed() {
        // The direction that has to be safe: `--path` on a subcommand with no
        // such flag is `unknown flag`, so a command that worked before riabuild
        // touched it stops working. Anything not on the list gets nothing.
        for typed in [
            vec!["ssh", "issue-credentials"],
            vec!["dynamic-secrets"],
            vec!["init"],
        ] {
            assert_eq!(scoped(&args(&typed), Some(&brokered())), args(&typed));
        }
    }

    #[test]
    fn a_command_with_no_credential_is_not_scoped_either() {
        // Nothing was brokered, so there is no team answer to fill in — and a
        // `--path` from riabuild beside no credential would only change which
        // folder infisical says it cannot read.
        assert_eq!(scoped(&args(&["export"]), None), args(&["export"]));
    }

    #[test]
    fn the_scope_goes_to_infisical_and_never_to_the_program_it_starts() {
        // `infisical run --path=/apps -- pnpm dev`, not `pnpm dev --path=/apps`
        // — which would be a flag pnpm has never heard of, on the command a
        // developer is most likely to be running when this matters.
        let scoped = scoped(&args(&["run", "--", "pnpm", "dev"]), Some(&brokered()));
        assert_eq!(
            scoped,
            args(&["run", "--env=dev", "--path=/apps", "--", "pnpm", "dev"])
        );
    }
}
