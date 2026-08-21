//! Paths as text: the `~` a developer types, and the `PATH` a shell splits on
//! `:`.
//!
//! String work rather than layout, and none of it touches the disk.
//! [`path_without`] is the one that breaks a machine when it is wrong — see its
//! own note.

use std::path::{Path, PathBuf};

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
