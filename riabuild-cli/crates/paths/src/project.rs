//! Where a repository is checked out.
//!
//! The one decision this crate takes from the operating system, which is why
//! `default_project_dir_on` takes the OS as a parameter and
//! [`default_project_dir`] is the thin wrapper that supplies the real one:
//! `cfg!` would compile every branch but the host's out of the test binary.

use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
