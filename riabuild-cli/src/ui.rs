//! Terminal output.
//!
//! The error shape is the important part: every failure says what was being
//! attempted in the developer's words, the exact command and its stderr, one
//! concrete next action, and whether re-running is safe. A provisioner that
//! fails vaguely is worse than one that does not run.

use std::io::{IsTerminal, Write};

pub struct Ui {
    colour: bool,
    quiet: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Ui {
    pub fn new(quiet: bool) -> Self {
        let colour = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { colour, quiet }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.colour {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn banner(&self, org: &str) {
        if self.quiet {
            return;
        }
        println!();
        println!(
            "{} {}",
            self.paint("1;34", "riabuild"),
            self.paint("2", &format!("· {org} environment")),
        );
    }

    pub fn heading(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("\n{}", self.paint("1", text));
    }

    /// A task that needed nothing.
    pub fn satisfied(&self, title: &str) {
        if self.quiet {
            return;
        }
        println!("  {} {}", self.paint("32", "●"), self.paint("2", title));
    }

    /// A task about to run, with the reason it is running.
    pub fn working(&self, title: &str, reason: &str) {
        if self.quiet {
            return;
        }
        print!(
            "  {} {} {}",
            self.paint("33", "◐"),
            title,
            self.paint("2", &format!("— {reason}")),
        );
        let _ = std::io::stdout().flush();
    }

    pub fn applied(&self, title: &str) {
        if self.quiet {
            return;
        }
        println!("\r  {} {}          ", self.paint("32", "●"), title);
    }

    pub fn note(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("    {}", self.paint("2", text));
    }

    pub fn warn(&self, text: &str) {
        eprintln!("  {} {}", self.paint("33", "▲"), text);
    }

    pub fn info(&self, text: &str) {
        if self.quiet {
            return;
        }
        println!("{text}");
    }

    /// The four things every failure must say.
    pub fn failure(&self, failure: &Failure) {
        eprintln!();
        eprintln!(
            "  {} {}",
            self.paint("1;31", "riabuild stopped:"),
            failure.attempting
        );
        if let Some(command) = &failure.command {
            eprintln!("    {} {}", self.paint("2", "ran"), command);
        }
        for line in failure
            .detail
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(8)
        {
            eprintln!("    {}", self.paint("2", line));
        }
        eprintln!("    {} {}", self.paint("1", "do this:"), failure.action);
        eprintln!(
            "    {}",
            self.paint(
                "2",
                if failure.safe_to_rerun {
                    "running `riabuild` again is safe once that is done"
                } else {
                    "do not re-run riabuild until that is done"
                },
            )
        );
        eprintln!();
    }
}

/// A failure a developer can act on.
#[derive(Debug, Clone)]
pub struct Failure {
    /// What riabuild was trying to do, in the developer's words.
    pub attempting: String,
    /// The exact command that failed, if there was one.
    pub command: Option<String>,
    /// stderr, or whatever else explains it.
    pub detail: String,
    /// One concrete next action.
    pub action: String,
    pub safe_to_rerun: bool,
}

impl Failure {
    pub fn new(attempting: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            attempting: attempting.into(),
            command: None,
            detail: String::new(),
            action: action.into(),
            safe_to_rerun: true,
        }
    }

    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.attempting, self.action)
    }
}

impl std::error::Error for Failure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_carries_all_four_parts() {
        let failure = Failure::new("checking your GitHub sign-in", "run `gh auth login`")
            .command("gh auth status")
            .detail("You are not logged into any GitHub hosts.");

        assert!(failure.command.is_some());
        assert!(!failure.detail.is_empty());
        assert!(failure.safe_to_rerun);
        assert_eq!(
            failure.to_string(),
            "checking your GitHub sign-in — run `gh auth login`"
        );
    }
}
