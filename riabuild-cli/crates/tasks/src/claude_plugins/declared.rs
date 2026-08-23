//! What one checkout asks Claude Code to load, read out of its own
//! `.claude/settings.json`.
//!
//! Nothing here decides *what* to install. It reads the same file Claude Code
//! would read, and refuses only the entries that could not be handed to
//! `claude` as arguments at all.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// What one checkout asks Claude Code to load.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Declared {
    /// Marketplace name → the source argument `plugin marketplace add` takes.
    /// Ordered so two runs issue the same commands in the same order.
    pub(super) marketplaces: BTreeMap<String, String>,
    /// The `<plugin>@<marketplace>` ids the settings switch on.
    pub(super) plugins: Vec<String>,
}

impl Declared {
    pub(super) fn is_empty(&self) -> bool {
        self.marketplaces.is_empty() && self.plugins.is_empty()
    }
}

/// Whether a string from the checkout's settings can be handed to `claude` as a
/// positional argument at all.
///
/// A marketplace source and a plugin id both arrive as JSON written by whoever
/// last edited the repository's `.claude/settings.json`, and both are spliced
/// into an argv. One beginning with `-` is read by the CLI's option parser
/// rather than as the value it stands in for: against Claude Code 2.1.235,
/// `claude plugin marketplace add --version` answers
/// `error: unknown option '--version'`, and other spellings would be options
/// that *are* known. That is the hazard class `api::Repo` exists to close for
/// `gh repo clone`, seen in a second place.
///
/// The `--` in `apply()` is the other half of the same fix and neither replaces
/// the other. `--` stops the parser from reading the value as an option — the
/// same command with `--` in front of it reaches the marketplace resolver and
/// is refused as a bad source — while this stops riabuild from asking for a
/// thing no legitimate declaration names. Belt and braces on purpose: `--`
/// depends on a CLI's parser behaving as commander's does, and this does not.
///
/// An empty string is refused too. Argv carries it perfectly well and it names
/// nothing, so it can only ever be a failed install.
fn nameable(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-')
}

/// The argument `claude plugin marketplace add` takes for one declaration.
///
/// `None` for a shape riabuild cannot name on a command line — a source kind
/// added after this was written, an entry missing the field its kind needs, or
/// one whose value would be read as an option rather than a source. Skipping is
/// deliberate and is not a silent failure mode: a marketplace this cannot
/// *install* must not become one `check()` demands, or the task fails forever on
/// a machine whose `apply()` could never satisfy it. Claude Code still installs
/// it in the background exactly as it did before.
pub(super) fn source_argument(entry: &Value) -> Option<String> {
    let source = entry.get("source")?;
    let named = |key: &str| source.get(key)?.as_str().map(str::to_string);
    // The CLI accepts the same shorthand a human types, so a bare string needs
    // no interpreting.
    let argument = if let Value::String(text) = source {
        text.clone()
    } else {
        match source.get("source")?.as_str()? {
            "github" => named("repo")?,
            "url" => named("url")?,
            "directory" | "file" => named("path")?,
            _ => return None,
        }
    };
    nameable(&argument).then_some(argument)
}

/// What the checkout declares, or nothing at all.
///
/// Every failure here — no file, unreadable, not JSON — reports nothing
/// declared rather than an error, because that is what Claude Code itself does
/// with the same file. A settings file the developer is midway through editing
/// is not a reason to refuse to provision their machine, and treating it as one
/// would make riabuild stricter about a checked-in file than the tool the file
/// is for.
pub(crate) async fn declared_in(dir: &Path) -> Declared {
    let file = dir.join(".claude").join("settings.json");
    let Ok(text) = tokio::fs::read_to_string(&file).await else {
        return Declared::default();
    };
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&text) else {
        return Declared::default();
    };

    let marketplaces = root
        .get("extraKnownMarketplaces")
        .and_then(Value::as_object)
        .map(|declared| {
            declared
                .iter()
                .filter_map(|(name, entry)| Some((name.clone(), source_argument(entry)?)))
                .collect()
        })
        .unwrap_or_default();

    // `false` is how a developer turns one off, and is as meaningful as its
    // absence.
    let plugins = root
        .get("enabledPlugins")
        .and_then(Value::as_object)
        .map(|enabled| {
            enabled
                .iter()
                .filter(|(_, on)| on.as_bool() == Some(true))
                .map(|(id, _)| id.clone())
                // Same reasoning as `source_argument`: a plugin id is spliced
                // into an argv too, and `check()` must not demand what
                // `apply()` is not willing to ask for.
                .filter(|id| nameable(id))
                .collect()
        })
        .unwrap_or_default();

    Declared {
        marketplaces,
        plugins,
    }
}
