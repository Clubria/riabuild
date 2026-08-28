//! Wrapping a runner, without copying the trait out by hand each time.
//!
//! Every wrapper riabuild has wants the same seven forwarding bodies and
//! differs in one or two of them: the environment scope below rewrites each
//! call's options, and the test doubles in `remote` and `channel` intercept one
//! command and pass the rest through. Written out by hand, a method added to
//! `CommandRunner` is an edit in each of them — and one of them had already
//! forgotten `spawn_piped`, so its "delegated like the rest" comment was true
//! of five methods and not of the sixth, which fell through to the trait's
//! refusing default rather than to the runner it wrapped.
//!
//! So the difference is a value rather than a copied `impl`. [`Delegating`]
//! forwards everything and is the only place that knows what "everything" is;
//! a [`Decoration`] is the part that differs.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::{BytesOutput, ChildHandle, CommandOutput, CommandRunner, PipedChildHandle, RunOptions};

/// What one wrapper does that the next one does not.
///
/// Both methods are defaulted, so a decoration states only its own half:
/// [`EnvScope`] below is a rewrite and no side effect, and a double that
/// intercepts a command is a side effect and no rewrite.
#[async_trait]
pub trait Decoration: Send + Sync + 'static {
    /// What each call's options become before the wrapped runner sees them.
    fn options(&self, options: &RunOptions) -> RunOptions {
        options.clone()
    }

    /// Runs before each call reaches the wrapped runner, and fails it on an
    /// error.
    ///
    /// `which` is the one entry point this does not cover: it is synchronous —
    /// it stats `PATH` candidates and nothing more — so there is nothing to
    /// await in front of it.
    async fn before(&self, _program: &str, _args: &[&str], _options: &RunOptions) -> Result<()> {
        Ok(())
    }
}

/// A runner wrapped around another, forwarding every method to it.
pub struct Delegating<D> {
    inner: Arc<dyn CommandRunner>,
    decoration: D,
}

impl<D: Decoration> Delegating<D> {
    pub fn around(inner: Arc<dyn CommandRunner>, decoration: D) -> Self {
        Self { inner, decoration }
    }

    /// The decoration, applied once per call, whichever method the call came
    /// in through.
    async fn enter(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<RunOptions> {
        self.decoration.before(program, args, options).await?;
        Ok(self.decoration.options(options))
    }
}

#[async_trait]
impl<D: Decoration> CommandRunner for Delegating<D> {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<CommandOutput> {
        let options = self.enter(program, args, options).await?;
        self.inner.run(program, args, &options).await
    }

    async fn run_bytes(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<BytesOutput> {
        let options = self.enter(program, args, options).await?;
        self.inner.run_bytes(program, args, &options).await
    }

    async fn run_forking(&self, program: &str, args: &[&str], options: &RunOptions) -> Result<i32> {
        let options = self.enter(program, args, options).await?;
        self.inner.run_forking(program, args, &options).await
    }

    async fn spawn(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn ChildHandle>> {
        let options = self.enter(program, args, options).await?;
        self.inner.spawn(program, args, &options).await
    }

    async fn spawn_piped(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<Box<dyn PipedChildHandle>> {
        let options = self.enter(program, args, options).await?;
        self.inner.spawn_piped(program, args, &options).await
    }

    async fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<()> {
        let options = self.enter(program, args, options).await?;
        self.inner.spawn_detached(program, args, &options).await
    }

    async fn exec_replacing(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        let options = self.enter(program, args, options).await?;
        self.inner.exec_replacing(program, args, &options).await
    }

    async fn run_interactive(
        &self,
        program: &str,
        args: &[&str],
        options: &RunOptions,
    ) -> Result<i32> {
        let options = self.enter(program, args, options).await?;
        self.inner.run_interactive(program, args, &options).await
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.inner.which(program)
    }
}

/// The decoration behind [`ScopedRunner`]: a fixed environment on every
/// command.
///
/// Whatever keys the scope was constructed with are applied *after* the
/// caller's `RunOptions.env`, so they are not overridable:
/// `std::process::Command::env()` (see `RealRunner::build`) overwrites on a
/// repeated key, and the scope's entries are the ones applied last. A task
/// cannot escape its namespace even by naming one of these keys itself —
/// accidentally (a copy-pasted env vector from another task) or otherwise.
/// Every other variable a caller sets — `env_local`'s `INFISICAL_TOKEN`, for
/// instance — has nothing here to collide with, so it reaches the child
/// untouched. See the precedence tests in `lib.rs`, including one that pins the
/// collision case: it is written to fail if the merge order were ever put back
/// the other way around.
pub struct EnvScope {
    env: Vec<(String, String)>,
}

impl Decoration for EnvScope {
    fn options(&self, options: &RunOptions) -> RunOptions {
        let mut merged = options.clone();
        let mut env = options.env.clone();
        env.extend(self.env.iter().cloned());
        merged.env = env;
        merged
    }
}

/// A `CommandRunner` that adds a fixed environment to every command.
///
/// This is why `github_cli` cannot authenticate the wrong developer on a shared
/// server. `GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` are not something each task
/// remembers to pass — the runner every task already holds carries them, so a
/// task that forgets is not a thing anyone can write.
///
/// The un-overridable set is exactly what `main.rs` puts in, and today that is
/// **`GH_CONFIG_DIR` and `GIT_CONFIG_GLOBAL` only**. `RIABUILD_ROOT` is *not*
/// in it: it reaches children by ordinary process-environment inheritance
/// instead, which the precedence rule on [`EnvScope`] does not cover, so a task
/// that put `RIABUILD_ROOT` in its own `RunOptions.env` would win over the
/// inherited value. No task does that today. Do not read this comment as saying
/// the namespace root is protected the way the two config paths are — if that
/// protection is ever wanted, `main.rs` has to add the key here.
///
/// Every method scopes, including the ones the laptop channel added: a
/// clipboard read through `run_bytes`, a clipboard write through
/// `run_forking`, and the tunnel held open by `spawn` all reach the child with
/// the same environment `run` would have given it, so no route through this
/// trait is an unscoped one. `spawn` matters most of the three — it is the
/// longest-lived child riabuild starts, so an unscoped one would keep pointing
/// at the wrong developer's configuration for the whole session. That is now a
/// property of [`Delegating`] rather than of six hand-written bodies here.
pub type ScopedRunner = Delegating<EnvScope>;

impl ScopedRunner {
    pub fn new(inner: Arc<dyn CommandRunner>, env: Vec<(String, String)>) -> Self {
        Self::around(inner, EnvScope { env })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeRunner;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A decoration whose whole job is a side effect: the shape of the test
    /// doubles that intercept one command and delegate the rest.
    struct Counting(Arc<AtomicUsize>);

    #[async_trait]
    impl Decoration for Counting {
        async fn before(&self, _: &str, _: &[&str], _: &RunOptions) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The property the hand-written wrappers could not have: every entry
    /// point is forwarded, and `Delegating` is the only place that has to know
    /// what "every" means. A wrapper that omits one does not fail to compile —
    /// it falls through to the trait's default, which for `spawn_piped`
    /// refuses — so this counts the calls rather than trusting the impl to be
    /// complete.
    #[tokio::test]
    async fn every_entry_point_goes_through_the_decoration() {
        let seen = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(FakeRunner::new());
        let wrapper = Delegating::around(inner.clone(), Counting(seen.clone()));
        let options = RunOptions::default();

        wrapper.run("gh", &[], &options).await.expect("run");
        wrapper
            .run_bytes("xclip", &[], &options)
            .await
            .expect("run_bytes");
        wrapper
            .run_forking("wl-copy", &[], &options)
            .await
            .expect("run_forking");
        wrapper.spawn("ssh", &[], &options).await.expect("spawn");
        wrapper
            .spawn_piped("ssh", &["-T"], &options)
            .await
            .expect("spawn_piped");
        wrapper
            .run_interactive("bash", &[], &options)
            .await
            .expect("run_interactive");

        assert_eq!(seen.load(Ordering::SeqCst), 6);
        assert_eq!(inner.calls().len(), 6, "{:?}", inner.calls());
    }

    /// A decoration that cannot do its half stops the call rather than letting
    /// it through undecorated.
    #[tokio::test]
    async fn a_decoration_that_fails_fails_the_call() {
        struct Refusing;

        #[async_trait]
        impl Decoration for Refusing {
            async fn before(&self, _: &str, _: &[&str], _: &RunOptions) -> Result<()> {
                anyhow::bail!("not this one")
            }
        }

        let inner = Arc::new(FakeRunner::new());
        let wrapper = Delegating::around(inner.clone(), Refusing);
        let error = wrapper
            .run("gh", &[], &RunOptions::default())
            .await
            .expect_err("the decoration refused");

        assert!(error.to_string().contains("not this one"), "{error}");
        assert!(
            inner.calls().is_empty(),
            "the call still reached the runner"
        );
    }
}
