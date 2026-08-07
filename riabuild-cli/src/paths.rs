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
    fn org_settings_file(&self) -> PathBuf {
        self.root().join("org-settings.json")
    }
    /// A server's own riabuild session. Never used on a laptop, where the
    /// platform keychain holds it instead.
    #[allow(dead_code)] // consumed by Task 10 (Task 9 only threads it through as an Option<PathBuf>)
    fn session_token_file(&self) -> PathBuf {
        self.root().join("session.token")
    }
    /// Who this namespace belongs to, in words, for whoever has a shell on the
    /// box and finds a directory named after a UUID.
    #[allow(dead_code)] // consumed by Task 18
    fn owner_file(&self) -> PathBuf {
        self.root().join("owner.json")
    }
    fn bin_dir(&self) -> PathBuf {
        self.root().join("bin")
    }
    fn node_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("node").join(version)
    }
    /// pnpm 11 and newer are a launcher plus the `dist/` tree it loads, so they
    /// get a directory of their own rather than a file in `bin/`.
    fn pnpm_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("pnpm").join(version)
    }
    #[allow(dead_code)] // consumed by Task 17
    fn riabuild_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("riabuild").join(version)
    }
    fn claude_dir(&self) -> PathBuf {
        self.root().join("claude")
    }
    fn shell_dir(&self, shell: &str) -> PathBuf {
        self.root().join("shell").join(shell)
    }
    fn log_file(&self) -> PathBuf {
        self.root().join("logs").join("riabuild.log")
    }
}

pub struct RealPaths {
    home: PathBuf,
    root: PathBuf,
}

impl RealPaths {
    pub fn new() -> anyhow::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
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
    #[allow(dead_code)] // consumed by Task 17, deriving remote_binary_path from riabuild_dir()
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
#[allow(dead_code)] // consumed by Task 18
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
/// why it lives in the file that is allowed to know about one. A Mac developer
/// keeps work in `~/Documents`, and finding a checkout in `~/code` on macOS
/// reads as riabuild dumping a folder somewhere arbitrary; `~/code` is the
/// convention everywhere else.
///
/// It used to be a single string sent by the server, which cannot be right on
/// both platforms at once. A developer who wants somewhere else passes
/// `riabuild --project <path>`, and that choice is remembered.
pub fn default_project_dir(home: &Path, repo_name: &str) -> PathBuf {
    default_project_dir_on(std::env::consts::OS, home, repo_name)
}

/// Split out so both platforms' answers are testable from either platform —
/// `cfg!` would compile one of these branches out of the test binary entirely.
fn default_project_dir_on(os: &str, home: &Path, repo_name: &str) -> PathBuf {
    match os {
        "macos" => home.join("Documents").join(ORG_DIR).join(repo_name),
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
        for os in ["linux", "freebsd"] {
            assert_eq!(
                default_project_dir_on(os, Path::new("/home/ada"), "ai-builders-hub"),
                PathBuf::from("/home/ada/code/ai-builders-hub"),
                "{os}"
            );
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
}
