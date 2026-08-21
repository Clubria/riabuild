//! What a server is called on this laptop.
//!
//! A name is a shell word a developer types (`riabuild remote gpu`) as well as
//! a label in a box, which is why a typed one is reduced rather than accepted
//! and why an unusable answer is asked for again a bounded number of times.

use riabuild_ui::Ui;

use super::DISPLAY_PREFIX;

/// How many times riabuild asks for a usable name before naming the server
/// itself. Bounded for the same reason `tasks::project`'s checkout prompt is:
/// a developer who cannot give a usable answer is better served by riabuild
/// picking one than by being asked forever.
const NAME_ATTEMPTS: usize = 3;

/// What a typed name is reduced to before it is used.
///
/// The name is not decoration. It is exported as `RIABUILD_REMOTE=<name>`
/// inside the single-quoted `env …` prefix every remote invocation is wrapped
/// in, and it is what `riabuild remote forget <name>` looks a server up by. So
/// it is held to what a shell word and a lookup key can both carry: letters,
/// digits, dot, dash, underscore. Anything else is dropped rather than
/// rejected, and the developer is told the name they actually got.
pub fn sanitise_name(typed: &str) -> String {
    typed
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

/// The developer's own label for the server being added, offered riabuild's
/// guess as the default.
///
/// `ask`, not `ask_required`: there is a sensible default here, so an
/// unattended run — `riabuild remote user@host --accept-host-key …` in CI, or
/// anything with no terminal — takes it silently rather than failing. That is
/// the crate rule in `CLAUDE.md`, and it is also what keeps this prompt from
/// turning a scripted `remote` invocation into a hang.
///
/// Why ask at all: the default is the first label of the hostname, which is
/// the machine's name only when a developer connects straight to the machine.
/// Behind a gateway it is the gateway's — every server reached through
/// `ssh.cloudcli.ai` wants to be called `ssh`, then `ssh-2`, then `ssh-3`, and
/// the list in `remote list` stops telling anyone which box is which.
pub fn ask_name(ui: &Ui, host: &str, taken: &[String]) -> String {
    let default = allocate_name(host, taken);
    for _ in 0..NAME_ATTEMPTS {
        // `None` is Enter, ^D, or nobody there — all of which mean "use the
        // one you suggested", so none of them cost the developer an attempt.
        let Some(answer) = ui.ask(&format!("Name this server [{default}]")) else {
            break;
        };
        let name = sanitise_name(&answer);
        if name.is_empty() {
            ui.warn("A name can hold letters, digits, dots, dashes and underscores.");
            continue;
        }
        if taken.iter().any(|other| other == &name) {
            // Not a cosmetic clash: `Store::find` returns the first match, so
            // two records under one name means `remote forget <name>` and
            // every later reconnect act on whichever was saved first.
            ui.warn(&format!(
                "{name} is already the name of another server. Pick another."
            ));
            continue;
        }
        if name.to_ascii_lowercase().starts_with(DISPLAY_PREFIX) {
            // Reserved: it is how the team's servers are shown, so a local one
            // wearing it would be two servers a developer cannot tell apart at
            // the prompt — the same reason a name already taken is refused.
            ui.warn(&format!(
                "Names starting with \"{DISPLAY_PREFIX}\" belong to the team's servers. Pick another."
            ));
            continue;
        }
        if name != answer.trim() {
            ui.note(&format!("This server will be known as {name}."));
        }
        return name;
    }
    ui.note(&format!("This server will be known as {default}."));
    default
}

/// A short local label, from the first label of the hostname.
pub fn allocate_name(host: &str, taken: &[String]) -> String {
    let base: String = host
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let base = if base.is_empty() {
        "server".to_string()
    } else {
        base
    };

    if !taken.iter().any(|name| name == &base) {
        return base;
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !taken.iter().any(|name| name == &candidate) {
            return candidate;
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_from_the_first_label_of_the_hostname() {
        assert_eq!(allocate_name("build-01.fly.dev", &[]), "build-01");
        assert_eq!(allocate_name("gpu.internal", &[]), "gpu");
        assert_eq!(allocate_name("192.168.1.10", &[]), "192");
    }

    #[test]
    fn a_taken_name_is_numbered_rather_than_reused() {
        let taken = vec!["build".to_string(), "build-2".to_string()];
        assert_eq!(allocate_name("build.example.com", &taken), "build-3");
    }

    #[test]
    fn a_hostname_with_nothing_usable_in_it_still_gets_a_name() {
        assert_eq!(allocate_name("", &[]), "server");
        assert_eq!(allocate_name("...", &[]), "server");
    }

    #[test]
    fn a_typed_name_is_reduced_to_what_a_shell_word_can_carry() {
        assert_eq!(sanitise_name("  gpu-01 "), "gpu-01");
        assert_eq!(sanitise_name("bench.eu_west"), "bench.eu_west");
        // The name is interpolated into the single-quoted `env 'RIABUILD_REMOTE=…'`
        // prefix every remote invocation is wrapped in, so a quote or a space
        // in it is a broken command line, not a quirky label.
        let hostile = sanitise_name("gpu'; rm -rf /");
        assert!(
            !hostile.contains(['\'', ' ', ';', '/']),
            "{hostile} would not survive being quoted into a command"
        );
        assert_eq!(sanitise_name("  "), "");
        assert_eq!(sanitise_name("🚀"), "");
    }
}
