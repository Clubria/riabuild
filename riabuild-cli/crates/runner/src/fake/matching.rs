//! Which scripted response answers an invocation.
//!
//! Longest matching prefix wins, a queued response is consumed before the
//! standing stub, and a stub that names environment entries answers only a
//! call carrying all of them.

use std::collections::VecDeque;
use std::path::Path;

use crate::fake::{Ending, FakeRunner, Recorded, Stub};
use crate::options::RunOptions;
use crate::output::CommandOutput;

impl FakeRunner {
    pub(super) fn record(&self, program: &str, args: &[&str], options: &RunOptions) -> String {
        let invocation = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.calls.lock().unwrap().push(invocation.clone());
        self.recorded.lock().unwrap().push(Recorded {
            invocation: invocation.clone(),
            env: options.env.clone(),
            env_removed: options.env_remove.clone(),
            stdin: options.stdin.clone(),
            subdued: options.subdued.is_some(),
        });
        invocation
    }

    /// The invocation a stub is matched against.
    ///
    /// The program is reduced to its file name, because riabuild runs the tools
    /// it owns by absolute path — `~/.riabuild/gh/2.97.0/bin/gh`, under a
    /// per-test tempdir. Without this every stub would have to be built from a
    /// path the test does not care about, and `with("gh --version", …)` would
    /// stop meaning "when gh is asked its version".
    ///
    /// `calls` still records the full path, so a test can assert riabuild ran
    /// *its* gh rather than whatever is on `PATH`.
    pub(super) fn stub_key(program: &str, args: &[&str]) -> String {
        let name = Path::new(program)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string());
        format!("{name} {}", args.join(" ")).trim_end().to_string()
    }

    /// Pops the next response queued by `then()` for the longest matching
    /// key (same prefix rule as `responses`), leaving an exhausted queue in
    /// place so a later call matching the same key falls through to the
    /// ordinary stubs instead of finding nothing.
    pub(super) fn next_queued(&self, invocation: &str) -> Option<CommandOutput> {
        let mut sequenced = self.sequenced.lock().unwrap();
        let key = sequenced
            .keys()
            .filter(|key| invocation == key.as_str() || invocation.starts_with(&format!("{key} ")))
            .max_by_key(|key| key.len())
            .cloned()?;
        sequenced.get_mut(&key).and_then(VecDeque::pop_front)
    }

    /// Pops the next child queued for the longest matching key, by the same
    /// prefix rule the response stubs use — `spawning("ssh", …)` answers for
    /// the whole `ssh -T … ada@box riabuild channel pump` command line the
    /// supervisor builds, which no test should have to spell out to script an
    /// exit.
    pub(super) fn next_child(&self, invocation: &str) -> Option<Ending> {
        let mut children = self.children.lock().unwrap();
        let key = children
            .keys()
            .filter(|key| invocation == key.as_str() || invocation.starts_with(&format!("{key} ")))
            .max_by_key(|key| key.len())
            .cloned()?;
        children.get_mut(&key).and_then(VecDeque::pop_front)
    }

    /// Finds a stub for an invocation, by full program path or by file name.
    ///
    /// Both, because tasks run some binaries by absolute path and others by
    /// name, and a test should be able to say whichever it means. `toolchain`
    /// stubs the exact `~/.riabuild/node/<version>/bin/node` it is asserting
    /// about; `github_cli` says `gh --version` and does not care where gh is.
    ///
    /// A response queued by `then()` is consumed first, and consumed exactly
    /// once per call: the queue lookup that finds nothing pops nothing, so
    /// trying the file-name key after the full one cannot eat a second entry.
    pub(super) fn resolve(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Option<CommandOutput> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        let key = FakeRunner::stub_key(program, args);
        if let Some(output) = self
            .next_queued(&full)
            .or_else(|| self.next_queued(key.as_str()))
        {
            return Some(output);
        }
        self.stubbed(&full, options)
            .or_else(|| self.stubbed(&key, options))
    }

    pub(super) fn stubbed(&self, invocation: &str, options: &RunOptions) -> Option<CommandOutput> {
        self.matching(invocation, options)
            .map(|stub| stub.output.clone())
    }

    /// The stub an invocation selects, if any.
    ///
    /// Shared by the text and byte lookups so a binary stub can never be
    /// selected by different rules from the text one beside it.
    pub(super) fn matching(&self, invocation: &str, options: &RunOptions) -> Option<&Stub> {
        self.responses
            .iter()
            .filter(|stub| {
                let name_matches = if stub.fragment {
                    invocation.contains(stub.invocation.as_str())
                } else {
                    invocation == stub.invocation
                        || invocation.starts_with(&format!("{} ", stub.invocation))
                };
                name_matches
                    && stub
                        .env
                        .iter()
                        .all(|(key, value)| options.env.iter().any(|(k, v)| k == key && v == value))
            })
            // Most specific wins: the longest command, then the most
            // environment entries. `max_by_key` keeps the last of equal
            // candidates, so a later identical stub replaces an earlier one —
            // which is what the map this replaced did.
            //
            // Command length is compared before env-pair count, so a longer
            // env-less stub outranks a shorter env-scoped one: `with("claude
            // auth status --json")` beats `with_env("claude auth", &[("CLAUDE_
            // CONFIG_DIR", "/one")])` for `claude auth status --json` run in
            // `/one`. That is fine for the account use case, where every
            // account is asked the identical command string, but it means env
            // specificity only breaks ties between stubs that already match on
            // the same invocation length.
            //
            // Length is also what settles a fragment against a prefix: a longer
            // match is a more specific stub, so a prefix stub on `"ssh"` cannot
            // silently answer for every remote invocation a fragment stub
            // scripts. The last tuple element keeps a prefix stub ahead of a
            // fragment of the identical length, which is the direction the
            // fragment rule was originally written with.
            .max_by_key(|stub| {
                (
                    stub.invocation.len(),
                    stub.env.len(),
                    u8::from(!stub.fragment),
                )
            })
    }

    /// The byte-stub twin of `resolve`, matched by exactly the same rules so a
    /// binary stub cannot be selected differently from a text one.
    pub(super) fn resolve_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Option<Vec<u8>> {
        let full = format!("{program} {}", args.join(" "))
            .trim_end()
            .to_string();
        self.matching(&full, options)
            .or_else(|| self.matching(&FakeRunner::stub_key(program, args), options))
            .and_then(|stub| stub.bytes.clone())
    }

    pub(super) fn lookup(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> CommandOutput {
        self.resolve(program, args, options)
            .unwrap_or_else(|| CommandOutput {
                code: Some(127),
                stdout: String::new(),
                stderr: format!(
                    "fake runner: no stub for `{}`",
                    FakeRunner::stub_key(program, args)
                ),
            })
    }
}
