//! Where the root is.
//!
//! [`RealPaths`] is the machine's own answer; [`root_for`] is the decision
//! behind it, pure so it is testable without setting a process-wide environment
//! variable; and [`remote_namespace`] is the single definition of the
//! per-developer layout a shared server uses, which `remote/session.rs` also
//! needs as a `String` and calls rather than formats for itself.

use crate::Paths;
use riabuild_ui::Failure;
use std::path::{Path, PathBuf};

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
    #[cfg(any(test, feature = "testing"))]
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

#[cfg(test)]
mod tests {
    use super::*;

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
