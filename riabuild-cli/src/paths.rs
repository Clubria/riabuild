//! Where riabuild keeps its things.
//!
//! Behind a trait from the first commit so Linux support is an addition rather
//! than a rewrite, and so tests can point the whole tree at a tempdir.

use std::path::{Path, PathBuf};

pub trait Paths: Send + Sync {
    /// `~/.riabuild`
    fn root(&self) -> PathBuf;
    /// The developer's real home, for locating their own shell rcfiles.
    fn home(&self) -> PathBuf;

    fn state_file(&self) -> PathBuf {
        self.root().join("state.json")
    }
    fn config_file(&self) -> PathBuf {
        self.root().join("config.json")
    }
    fn org_settings_file(&self) -> PathBuf {
        self.root().join("org-settings.json")
    }
    fn bin_dir(&self) -> PathBuf {
        self.root().join("bin")
    }
    fn node_dir(&self, version: &str) -> PathBuf {
        self.root().join("node").join(version)
    }
    /// pnpm 11 and newer are a launcher plus the `dist/` tree it loads, so they
    /// get a directory of their own rather than a file in `bin/`.
    fn pnpm_dir(&self, version: &str) -> PathBuf {
        self.root().join("pnpm").join(version)
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
}

impl RealPaths {
    pub fn new() -> anyhow::Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow::anyhow!("riabuild could not work out your home directory (is $HOME set?)")
        })?;
        Ok(Self { home })
    }

    #[cfg(test)]
    pub fn rooted_at(home: impl AsRef<Path>) -> Self {
        Self {
            home: home.as_ref().to_path_buf(),
        }
    }
}

impl Paths for RealPaths {
    fn root(&self) -> PathBuf {
        self.home.join(".riabuild")
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }
}

/// The folder Clubria checkouts are grouped under on macOS.
const MACOS_ORG_DIR: &str = "Clubria";

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
        "macos" => home.join("Documents").join(MACOS_ORG_DIR).join(repo_name),
        _ => home.join("code").join(repo_name),
    }
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
}
