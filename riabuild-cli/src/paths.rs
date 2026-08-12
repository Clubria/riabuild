//! Where riabuild keeps its things.
//!
//! Behind a trait from the first commit so Linux support is an addition rather
//! than a rewrite, and so tests can point the whole tree at a tempdir.

use std::path::{Path, PathBuf};

use crate::ui::Failure;

pub trait Paths: Send + Sync {
    /// This developer's own state. `~/.riabuild` on a laptop; on a shared
    /// server it is namespaced per developer — see [`root_for`].
    fn root(&self) -> PathBuf;
    /// The developer's real home, for locating their own shell rcfiles.
    fn home(&self) -> PathBuf;

    /// Tools everyone on this machine shares: node, pnpm, gh, infisical, and
    /// riabuild itself. Equal to `root()` on a laptop; on a server it stays at
    /// `~/.riabuild` while `root()` moves into a per-developer namespace, so one
    /// Unix account holds one toolchain and several developers.
    fn tools_root(&self) -> PathBuf {
        self.root()
    }

    fn state_file(&self) -> PathBuf {
        self.root().join("state.json")
    }
    fn config_file(&self) -> PathBuf {
        self.root().join("config.json")
    }
    /// Guards a read-modify-write of `state.json`, `config.json` or
    /// `remotes.json`. Held for milliseconds.
    ///
    /// Deliberately none of those files. Writes land by `rename`, so a lock
    /// taken on the data file would be a lock on an inode the next write
    /// unlinks — the following process would lock a fresh inode, see no
    /// contention, and proceed. A lock's identity has to outlive the data it
    /// guards, and that failure is invisible to every single-process test.
    fn state_lock_file(&self) -> PathBuf {
        self.root().join(".state.lock")
    }
    /// Guards the provisioning phase, so two runs do not install one toolchain
    /// twice. Held for seconds to minutes, and never across the shell handoff.
    ///
    /// Separate from `state_lock_file` because a run holding this one saves
    /// state after every task, and `std` is explicit that a second lock taken
    /// by a process that already holds one is unspecified and may deadlock.
    fn provision_lock_file(&self) -> PathBuf {
        self.root().join(".provision.lock")
    }
    fn org_settings_file(&self) -> PathBuf {
        self.root().join("org-settings.json")
    }
    /// A server's own riabuild session. Never used on a laptop, where the
    /// platform keychain holds it instead.
    fn session_token_file(&self) -> PathBuf {
        self.root().join("session.token")
    }
    /// Who this namespace belongs to, in words, for whoever has a shell on the
    /// box and finds a directory named after a UUID.
    fn owner_file(&self) -> PathBuf {
        self.root().join("owner.json")
    }
    fn bin_dir(&self) -> PathBuf {
        self.root().join("bin")
    }
    /// The Claude Code status line script. It sits beside `org-settings.json`
    /// rather than in `bin/` because it is a Node script Claude Code runs by
    /// name, not something that belongs on `PATH`. The org settings name this
    /// path as `node ~/.riabuild/claude-statusline.js`, so it cannot move
    /// without them.
    fn claude_statusline_file(&self) -> PathBuf {
        self.root().join("claude-statusline.js")
    }
    fn node_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("node").join(version)
    }
    /// pnpm 11 and newer are a launcher plus the `dist/` tree it loads, so they
    /// get a directory of their own rather than a file in `bin/`.
    fn pnpm_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("pnpm").join(version)
    }
    fn riabuild_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("riabuild").join(version)
    }
    /// `~/.riabuild/<tool>/<version>` — an owned copy of a third-party CLI.
    ///
    /// Versioned, so bumping a pin installs beside the old copy rather than
    /// writing over a binary that may be running.
    fn tool_dir(&self, tool: &str, version: &str) -> PathBuf {
        self.root().join(tool).join(version)
    }
    fn claude_dir(&self) -> PathBuf {
        self.root().join("claude")
    }
    /// One developer's Claude Code profile — what `CLAUDE_CONFIG_DIR` points at.
    fn claude_profile_dir(&self, profile: &str) -> PathBuf {
        self.claude_dir().join(profile)
    }
    /// Claude Code's own state for that profile. Named by Claude Code, not by
    /// riabuild: it puts `.claude.json` inside whatever `CLAUDE_CONFIG_DIR` is.
    fn claude_config_file(&self, profile: &str) -> PathBuf {
        self.claude_profile_dir(profile).join(".claude.json")
    }
    fn shell_dir(&self, shell: &str) -> PathBuf {
        self.root().join("shell").join(shell)
    }
    fn log_file(&self) -> PathBuf {
        self.root().join("logs").join("riabuild.log")
    }
    /// The servers this laptop knows about — see `remote::store`.
    fn remotes_file(&self) -> PathBuf {
        self.root().join("remotes.json")
    }
    /// The private key riabuild makes for each server, one file per
    /// `Remote::hash()`. Never shared with anything else riabuild writes;
    /// see `remote::identity`.
    fn identity_dir(&self) -> PathBuf {
        self.root().join("ssh-identities")
    }
    /// Where riabuild's own `known_hosts` lives — never the developer's
    /// `~/.ssh/known_hosts`. See `remote::identity::ssh_options`'s `-F
    /// /dev/null`, which is what makes that true.
    fn ssh_dir(&self) -> PathBuf {
        self.root().join("ssh")
    }
    fn known_hosts_file(&self) -> PathBuf {
        self.ssh_dir().join("known_hosts")
    }
    /// The `SSH_ASKPASS` helper riabuild points `ssh` at, so a password for a
    /// server is asked for once rather than at every one of the connections a
    /// single `riabuild remote` opens. Written on every run — see
    /// `remote::askpass::ensure_helper`.
    fn askpass_helper(&self) -> PathBuf {
        self.ssh_dir().join("askpass")
    }
    /// Where a saved SSH password lands on a machine with **no keyring at
    /// all**. The keychain is preferred everywhere it exists; see
    /// `keychain::select_password_store`, which owns that decision, and the
    /// amended "No secrets in `~/.riabuild/`" note in `CLAUDE.md`.
    fn remote_password_file(&self, hash: &str) -> PathBuf {
        self.ssh_dir().join("passwords").join(hash)
    }
}

pub struct RealPaths {
    home: PathBuf,
    root: PathBuf,
}

impl RealPaths {
    pub fn new() -> anyhow::Result<Self> {
        // `std::env::home_dir` rather than the `dirs` crate. It was deprecated
        // for years over a Windows bug riabuild could never hit, which is why
        // that crate existed here at all; the fix landed and it was
        // un-deprecated in Rust 1.86, well under this crate's 2024 edition
        // floor. On macOS and Linux it is `$HOME`, then the passwd entry —
        // exactly what `dirs` wrapped three crates around.
        let home = std::env::home_dir().ok_or_else(|| {
            anyhow::anyhow!("riabuild could not work out your home directory (is $HOME set?)")
        })?;
        let root = root_for(&home, std::env::var("RIABUILD_ROOT").ok().as_deref())?;
        Ok(Self { home, root })
    }

    /// A root chosen for this developer already — used on a shared server,
    /// where `root` is a per-developer namespace under `~/.riabuild-remote/`.
    ///
    /// Also how a layout method (`riabuild_dir`, `owner_file`, ...) gets evaluated
    /// against a *remote* home from the laptop side: root it here rather than
    /// formatting that layout a second time.
    pub fn with_root(home: impl AsRef<Path>, root: impl AsRef<Path>) -> Self {
        Self {
            home: home.as_ref().to_path_buf(),
            root: root.as_ref().to_path_buf(),
        }
    }

    /// The laptop shape: root and home coincide at `~/.riabuild`. Delegates to
    /// `with_root` rather than repeating the struct literal, so there is exactly
    /// one definition of "construct a `RealPaths`".
    #[cfg(test)]
    pub fn rooted_at(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref().to_path_buf();
        let root = home.join(".riabuild");
        Self::with_root(home, root)
    }
}

impl Paths for RealPaths {
    fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn tools_root(&self) -> PathBuf {
        self.home.join(".riabuild")
    }
}

/// Where riabuild keeps this developer's state.
///
/// Split out and pure so the decision is testable without setting an environment
/// variable every other test in the binary would then see.
///
/// An override that is not absolute is an **error**, never a fallback. It is set
/// by a laptop provisioning a server, and if it arrives unusable, defaulting to
/// `~/.riabuild` would put every developer on that box in one namespace —
/// sharing one session token, and therefore brokering secrets at each other's
/// role. Failing loudly is the only safe direction.
pub fn root_for(home: &Path, override_root: Option<&str>) -> anyhow::Result<PathBuf> {
    match override_root {
        None => Ok(home.join(".riabuild")),
        Some(path) if Path::new(path).is_absolute() => Ok(PathBuf::from(path)),
        // RIABUILD_ROOT deliberately appears in both `action` and `.detail()`: `action`
        // is part of `Failure`'s `Display`, so it is what any consumer that only sees
        // `to_string()` (including `anyhow::Error::to_string()`) gets; `.detail()` is
        // extra context `ui::failure()` prints to the terminal but `Display` does not
        // render (see `ui.rs:249-253`).
        Some(path) => Err(Failure::new(
            "working out where riabuild keeps your files",
            "Run `riabuild remote <server>` from your laptop again — it sets RIABUILD_ROOT, \
             and it has set it wrong.",
        )
        .detail(format!("RIABUILD_ROOT={path:?} is not an absolute path"))
        .into()),
    }
}

/// One developer's namespace on a shared server.
///
/// The single definition of that layout. `remote/session.rs` needs the same path
/// as a `String`, for a remote command rather than a local `PathBuf`, and calls
/// this rather than formatting its own — two spellings of one layout is how the
/// two drift apart, and one of them is what `rm -rf` is pointed at.
pub fn remote_namespace(home: &Path, member_id: &str) -> PathBuf {
    home.join(".riabuild-remote").join(member_id)
}

/// The folder Clubria checkouts are grouped under — under `Documents` on
/// macOS, and under a developer's own directory on a server (see
/// [`remote_project_dir`]). No longer macOS-only, hence the name.
const ORG_DIR: &str = "Clubria";

/// Where a repository is checked out when the developer has not chosen a place.
///
/// This is the only decision riabuild makes from the operating system, which is
/// why it lives in the file that is allowed to know about one.
///
/// | | |
/// |---|---|
/// | macOS | `~/Documents/Clubria/<repo>` |
/// | Linux | `~/Clubria/<repo>` |
/// | anything else | `~/code/<repo>` |
///
/// A Mac developer keeps work in `~/Documents`, and finding a checkout in
/// `~/code` on macOS reads as riabuild dumping a folder somewhere arbitrary.
/// Linux has no `~/Documents` worth speaking of, so the same organisation
/// grouping sits directly in the home directory.
///
/// The repository name comes from the org config's slug, so none of this is
/// tied to one repository.
///
/// It used to be a single string sent by the server, which cannot be right on
/// every platform at once. A developer who wants somewhere else passes
/// `riabuild --project <path>`, and that choice is remembered.
pub fn default_project_dir(home: &Path, repo_name: &str) -> PathBuf {
    default_project_dir_on(std::env::consts::OS, home, repo_name)
}

/// Split out so every platform's answer is testable from any platform —
/// `cfg!` would compile all but one of these branches out of the test binary.
fn default_project_dir_on(os: &str, home: &Path, repo_name: &str) -> PathBuf {
    match os {
        "macos" => home.join("Documents").join(ORG_DIR).join(repo_name),
        "linux" => home.join(ORG_DIR).join(repo_name),
        _ => home.join("code").join(repo_name),
    }
}

/// Where a checkout lands on a server.
///
/// Grouped by GitHub login because a developer `cd`s into this every day and a
/// UUID is not a path anyone can read. Nothing durable rests on the name: the
/// absolute path is recorded in the namespace's `config.json` the first time it
/// is chosen, so a later GitHub rename changes nothing.
///
/// Never `~/Documents`, on any platform: macOS protects it from SSH sessions,
/// returning "Operation not permitted" unless sshd has Full Disk Access, and
/// one answer on every platform is one less branch to be wrong in.
pub fn remote_project_dir(home: &Path, login: &str, repo_name: &str) -> PathBuf {
    home.join(ORG_DIR).join(login).join(repo_name)
}

/// Expands a leading `~` the way a developer typing a path expects.
pub fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// The reverse, for printing paths back without leaking the developer's name.
pub fn contract_tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// `PATH` with our own directory removed.
///
/// `~/.riabuild/bin` leads `PATH` inside the environment shell, and the shim
/// lives there under the same name as the tool it shadows. Resolving the real
/// binary against an unmodified `PATH` finds the shim again and `exec`s it
/// forever — a hard hang on the developer's server with no output at all. This
/// is the single most likely way to break someone's machine and it is one line
/// to get wrong.
pub fn path_without(path: &str, ours: &Path) -> String {
    let ours = ours.to_string_lossy();
    let ours = ours.trim_end_matches('/');

    let kept: Vec<&str> = path
        .split(':')
        .filter(|entry| !entry.is_empty() && entry.trim_end_matches('/') != ours)
        .collect();

    if kept.is_empty() {
        // An empty PATH is read as "." by some shells, which would resolve
        // whatever happens to be in the working directory.
        return "/usr/local/bin:/usr/bin:/bin".to_string();
    }
    kept.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_and_contracts_home() {
        let home = Path::new("/Users/ada");
        assert_eq!(
            expand_tilde("~/code/hub", home),
            PathBuf::from("/Users/ada/code/hub")
        );
        assert_eq!(expand_tilde("~", home), PathBuf::from("/Users/ada"));
        assert_eq!(expand_tilde("/tmp/x", home), PathBuf::from("/tmp/x"));
        assert_eq!(
            contract_tilde(Path::new("/Users/ada/code/hub"), home),
            "~/code/hub"
        );
        assert_eq!(contract_tilde(Path::new("/opt/hub"), home), "/opt/hub");
    }

    #[test]
    fn a_mac_checkout_lands_under_documents() {
        assert_eq!(
            default_project_dir_on("macos", Path::new("/Users/ada"), "ai-builders-hub"),
            PathBuf::from("/Users/ada/Documents/Clubria/ai-builders-hub")
        );
    }

    #[test]
    fn a_server_checkout_is_grouped_by_developer_and_avoids_documents() {
        // Not ~/Documents even on macOS: over SSH that directory is TCC-protected
        // and returns "Operation not permitted" unless sshd has Full Disk Access.
        // One answer on every platform is also one less branch to be wrong in.
        assert_eq!(
            remote_project_dir(Path::new("/home/dev"), "ada", "ai-builders-hub"),
            PathBuf::from("/home/dev/Clubria/ada/ai-builders-hub")
        );
        assert_eq!(
            remote_project_dir(Path::new("/Users/dev"), "bob", "ai-builders-hub"),
            PathBuf::from("/Users/dev/Clubria/bob/ai-builders-hub")
        );
    }

    #[test]
    fn a_linux_checkout_lands_under_the_org_directory() {
        // The same grouping macOS puts inside ~/Documents, minus the
        // ~/Documents that Linux does not really have.
        assert_eq!(
            default_project_dir_on("linux", Path::new("/home/ada"), "ai-builders-hub"),
            PathBuf::from("/home/ada/Clubria/ai-builders-hub")
        );
    }

    #[test]
    fn two_developers_sharing_a_server_get_different_checkouts() {
        // Same home, different GitHub logins — isolates login as the variable
        // that must produce different paths, which is the whole point of
        // grouping by developer: two co-tenants must never resolve to one
        // checkout.
        let home = Path::new("/home/dev");
        let ada = remote_project_dir(home, "ada", "ai-builders-hub");
        let bob = remote_project_dir(home, "bob", "ai-builders-hub");
        assert_ne!(ada, bob);
    }

    #[test]
    fn everywhere_else_uses_the_code_directory() {
        for os in ["freebsd", "openbsd"] {
            assert_eq!(
                default_project_dir_on(os, Path::new("/home/ada"), "ai-builders-hub"),
                PathBuf::from("/home/ada/code/ai-builders-hub"),
                "{os}"
            );
        }
    }

    #[test]
    fn the_repository_name_is_never_hardcoded() {
        // The slug comes from the org config, so pointing riabuild at another
        // repository must not still say ai-builders-hub.
        for os in ["macos", "linux", "freebsd"] {
            let dir = default_project_dir_on(os, Path::new("/home/ada"), "some-other-repo");
            assert!(dir.ends_with("some-other-repo"), "{os}: {dir:?}");
        }
    }

    #[test]
    fn the_default_matches_the_platform_it_is_running_on() {
        // Guards the wiring between the public function and the tested one.
        let home = Path::new("/home/ada");
        assert_eq!(
            default_project_dir(home, "hub"),
            default_project_dir_on(std::env::consts::OS, home, "hub")
        );
    }

    #[test]
    fn layout_is_rooted_under_riabuild() {
        let paths = RealPaths::rooted_at("/Users/ada");
        assert_eq!(paths.root(), PathBuf::from("/Users/ada/.riabuild"));
        assert_eq!(
            paths.node_dir("22.23.1"),
            PathBuf::from("/Users/ada/.riabuild/node/22.23.1")
        );
        assert!(paths.state_file().ends_with("state.json"));
    }

    #[test]
    fn a_laptop_keeps_one_root_for_everything() {
        let paths = RealPaths::rooted_at("/Users/ada");
        assert_eq!(paths.root(), PathBuf::from("/Users/ada/.riabuild"));
        assert_eq!(paths.tools_root(), paths.root());
        assert_eq!(
            paths.node_dir("22.23.1"),
            PathBuf::from("/Users/ada/.riabuild/node/22.23.1")
        );
    }

    #[test]
    fn a_server_namespaces_state_but_shares_tools() {
        let home = Path::new("/home/dev");
        let root = remote_namespace(home, "550e8400-e29b-41d4-a716-446655440000");
        let paths = RealPaths::with_root(home, &root);

        // State is one developer's.
        assert_eq!(
            paths.state_file(),
            PathBuf::from(
                "/home/dev/.riabuild-remote/550e8400-e29b-41d4-a716-446655440000/state.json"
            )
        );
        assert!(paths.claude_dir().starts_with(&root));
        assert!(paths.shell_dir("zsh").starts_with(&root));
        assert!(paths.bin_dir().starts_with(&root));
        assert_eq!(paths.session_token_file(), root.join("session.token"));

        // Tools are everybody's.
        assert_eq!(paths.tools_root(), PathBuf::from("/home/dev/.riabuild"));
        assert_eq!(
            paths.node_dir("22.23.1"),
            PathBuf::from("/home/dev/.riabuild/node/22.23.1")
        );
        assert_eq!(
            paths.riabuild_dir("2026.08.06"),
            PathBuf::from("/home/dev/.riabuild/riabuild/2026.08.06")
        );
    }

    #[test]
    fn the_root_override_is_read_without_touching_the_environment() {
        // Pure, so the decision is testable without setting a process-wide variable
        // every other test in this binary would then see.
        let home = Path::new("/home/dev");
        assert_eq!(
            root_for(home, None).expect("no override"),
            PathBuf::from("/home/dev/.riabuild")
        );
        assert_eq!(
            root_for(home, Some("/home/dev/.riabuild-remote/abc")).expect("absolute"),
            PathBuf::from("/home/dev/.riabuild-remote/abc")
        );
    }

    #[test]
    fn a_root_override_that_is_not_absolute_stops_rather_than_defaulting() {
        // The catastrophic case, and the reason this is an error and not a
        // fallback. `RIABUILD_ROOT` is set by the laptop when it provisions a
        // server. If it arrives unusable — an unexpanded `~`, an empty string —
        // defaulting to `~/.riabuild` puts *every* developer on that box in one
        // namespace: one session.token, so a candidate's riabuild brokers Infisical
        // at a lead's role, and one gh configuration, which is the silent
        // wrong-identity bug the whole design exists to prevent.
        let home = Path::new("/home/dev");
        for bad in ["", "relative/path", "~/.riabuild-remote/abc"] {
            let error = root_for(home, Some(bad)).expect_err(bad);
            assert!(
                error.to_string().contains("RIABUILD_ROOT"),
                "{bad:?} produced {error}"
            );
        }
    }

    /// The hard hang. `~/.riabuild/bin` leads PATH, so a naive search finds the
    /// shim itself and execs it forever.
    #[test]
    fn our_own_directory_is_stripped_before_the_real_binary_is_resolved() {
        let stripped = path_without(
            "/home/ada/.riabuild/bin:/usr/local/bin:/usr/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(stripped, "/usr/local/bin:/usr/bin");
    }

    #[test]
    fn stripping_handles_trailing_slashes_and_repeats() {
        let stripped = path_without(
            "/home/ada/.riabuild/bin/:/usr/bin:/home/ada/.riabuild/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(stripped, "/usr/bin");
    }

    /// A PATH that was only ever our directory must not become an empty string,
    /// which some shells read as ".".
    #[test]
    fn stripping_everything_leaves_a_safe_default_rather_than_an_empty_path() {
        let stripped = path_without(
            "/home/ada/.riabuild/bin",
            Path::new("/home/ada/.riabuild/bin"),
        );
        assert_eq!(stripped, "/usr/local/bin:/usr/bin:/bin");
    }
}
