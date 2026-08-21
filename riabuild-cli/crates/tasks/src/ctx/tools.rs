//! How a task names a binary riabuild owns.
//!
//! Every one of these is an absolute path into `~/.riabuild/`, because the
//! bare name resolves against a `PATH` riabuild did not choose — and during
//! provisioning does not contain `~/.riabuild/bin` at all.

use crate::Ctx;

impl Ctx {
    /// The `gh` riabuild owns.
    ///
    /// Every call site runs *this* rather than the string `"gh"`. Resolving
    /// through `PATH` would find whatever the developer happens to have, which
    /// is not the binary any `check()` verified — and during provisioning
    /// `~/.riabuild/bin` is not on `PATH` at all, so it would usually not find
    /// the owned copy even when one is installed.
    pub fn gh(&self) -> String {
        self.owned_tool(
            "gh",
            riabuild_fetch::tools::GH_VERSION,
            riabuild_fetch::tools::GH_MEMBER,
        )
    }

    /// The `infisical` riabuild owns. Same reasoning as `gh`.
    pub fn infisical(&self) -> String {
        self.owned_tool(
            "infisical",
            riabuild_fetch::tools::INFISICAL_VERSION,
            riabuild_fetch::tools::INFISICAL_MEMBER,
        )
    }

    /// The `ngrok` riabuild owns. Same reasoning as `gh`, with one addition:
    /// what `PATH` finds inside the environment shell is the *shim*, which is
    /// this binary plus the team's authtoken. riabuild's own `check()` wants
    /// the binary itself, unauthenticated and unwrapped.
    pub fn ngrok(&self) -> String {
        self.owned_tool(
            "ngrok",
            riabuild_fetch::tools::NGROK_VERSION,
            riabuild_fetch::tools::NGROK_MEMBER,
        )
    }

    /// The Grok Build riabuild owns. Same reasoning as `gh`.
    ///
    /// Note what this is *not*: `Ctx::claude()` and `Ctx::codex()` both build a
    /// path under the pinned Node, because those two are npm packages, and both
    /// fall back to a bare name before a Node is pinned. Grok Build is a static
    /// binary riabuild downloads whole, so it is an owned tool like `gh` and
    /// `ngrok` — always an absolute path, with no Node in the picture and no
    /// bare-name fallback for a launcher to defend against.
    pub fn grok(&self) -> String {
        self.owned_tool(
            "grok",
            riabuild_fetch::tools::GROK_VERSION,
            riabuild_fetch::tools::GROK_MEMBER,
        )
    }

    /// The Claude Code riabuild installed, by absolute path.
    ///
    /// Same reasoning as `gh()`, with one addition: `which("claude")` reads the
    /// ambient `PATH`, which during provisioning does not contain riabuild's
    /// Node — so it finds whatever the developer happens to have installed, or
    /// nothing at all in the moment just after riabuild installed one. Claude
    /// Code is installed by riabuild's own npm, so its home is the pinned
    /// Node's `bin`.
    ///
    /// Falls back to the bare name before a Node is pinned, which is the only
    /// thing a machine with no toolchain yet could use.
    pub fn claude(&self) -> String {
        match &self.config.node_version {
            Some(version) => self
                .paths
                .node_dir(version)
                .join("bin")
                .join("claude")
                .to_string_lossy()
                .into_owned(),
            None => "claude".to_string(),
        }
    }

    /// The Codex CLI riabuild installed, by absolute path.
    ///
    /// Same reasoning as `claude()`, and the same fallback: Codex is installed
    /// by riabuild's own npm, so its home is the pinned Node's `bin`, and
    /// before a Node is pinned the bare name is the only thing a machine could
    /// use.
    pub fn codex(&self) -> String {
        match &self.config.node_version {
            Some(version) => self
                .paths
                .node_dir(version)
                .join("bin")
                .join("codex")
                .to_string_lossy()
                .into_owned(),
            None => "codex".to_string(),
        }
    }

    fn owned_tool(&self, tool: &str, version: &str, member: &str) -> String {
        self.paths
            .tool_dir(tool, version)
            .join(member)
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::ctx_with;
    use riabuild_runner::FakeRunner;

    #[tokio::test]
    async fn claude_is_the_one_riabuilds_node_installed() {
        let (mut ctx, _home) = ctx_with(FakeRunner::new()).await;
        ctx.config.node_version = Some("22.23.1".into());
        let claude = ctx.claude();
        assert!(claude.ends_with("/node/22.23.1/bin/claude"), "{claude}");
        assert!(claude.starts_with(&ctx.paths.root().to_string_lossy().into_owned()));
    }

    #[tokio::test]
    async fn without_a_pinned_node_the_bare_name_is_all_there_is() {
        let (ctx, _home) = ctx_with(FakeRunner::new()).await;
        assert_eq!(ctx.claude(), "claude");
    }
}
