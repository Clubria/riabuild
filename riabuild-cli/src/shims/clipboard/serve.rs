//! Running the shim: talk to the channel, print in the tool's own format, and
//! never `exec` ourselves.

use super::{Intent, Tool, parse};
use crate::channel::client;
use crate::channel::mime::{self, Vocabulary};
use crate::channel::protocol::{Request, Response};
use crate::runner::{CommandRunner, RunOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the shim will print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Targets(Vec<String>),
    Bytes(Vec<u8>),
    Nothing,
}

fn vocabulary(tool: Tool) -> Vocabulary {
    match tool {
        Tool::Xclip => Vocabulary::X11,
        Tool::WlPaste => Vocabulary::Wayland,
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

/// The channel speaks MIME; each tool's callers expect that tool's vocabulary.
pub fn render(tool: Tool, output: &Output) -> Vec<u8> {
    match output {
        Output::Nothing => Vec::new(),
        Output::Bytes(bytes) => bytes.clone(),
        Output::Targets(targets) => {
            let vocab = vocabulary(tool);
            let mut out = String::new();
            for target in targets {
                if let Some(native) = mime::from_mime(vocab, target) {
                    out.push_str(native);
                    out.push('\n');
                }
            }
            out.into_bytes()
        }
    }
}

/// Runs the shim. Returns the exit code the real tool would have used.
///
/// "Channel down" and "clipboard empty" are deliberately identical to the
/// caller: xclip has no way to say "your laptop is asleep", and Claude Code
/// discards stderr. The distinction lives in the log.
pub async fn run(
    tool: Tool,
    args: &[String],
    socket: Option<PathBuf>,
    bin_dir: &Path,
    runner: &Arc<dyn CommandRunner>,
) -> i32 {
    let intent = parse(tool, args);

    let request = match &intent {
        Intent::PassThrough => return pass_through(tool, args, bin_dir, runner).await,
        Intent::Empty => return emit(tool, &Output::Nothing),
        Intent::Targets => Request::ClipboardTargets,
        Intent::Read(Some(target)) => match mime::to_mime(vocabulary(tool), target) {
            Some(mime) => Request::ClipboardRead { mime: mime.into() },
            // A type the channel does not carry reads as an empty clipboard,
            // which is what the real tool does for a target the selection does
            // not hold.
            None => return emit(tool, &Output::Nothing),
        },
        // No type named: ask what is there, then take the first — which is the
        // preferred text flavour when text is present.
        Intent::Read(None) => Request::ClipboardTargets,
    };

    let Some(socket) = socket else {
        log("no clipboard channel is configured for this session");
        return emit(tool, &Output::Nothing);
    };

    let reply = match client::request(&socket, &request).await {
        Ok(reply) => reply,
        Err(error) => {
            log(&format!("{error:#}"));
            return emit(tool, &Output::Nothing);
        }
    };

    match (&intent, reply.response) {
        (Intent::Targets, Response::Targets(targets)) => emit(tool, &Output::Targets(targets)),
        (Intent::Read(None), Response::Targets(targets)) => {
            let Some(first) = targets.first().cloned() else {
                return emit(tool, &Output::Nothing);
            };
            match client::request(&socket, &Request::ClipboardRead { mime: first }).await {
                Ok(second) => match second.response {
                    Response::Payload { .. } => emit(tool, &Output::Bytes(second.body)),
                    other => {
                        log(&describe(&other));
                        emit(tool, &Output::Nothing)
                    }
                },
                Err(error) => {
                    log(&format!("{error:#}"));
                    emit(tool, &Output::Nothing)
                }
            }
        }
        (_, Response::Payload { .. }) => emit(tool, &Output::Bytes(reply.body)),
        (_, other) => {
            log(&describe(&other));
            emit(tool, &Output::Nothing)
        }
    }
}

fn describe(response: &Response) -> String {
    match response {
        Response::Error { code, message } => format!("{}: {message}", code.as_str()),
        other => format!("unexpected reply: {other:?}"),
    }
}

/// Writes to stdout and returns the exit code.
///
/// Empty output exits 1, which is what both real tools do when the selection
/// holds nothing.
fn emit(tool: Tool, output: &Output) -> i32 {
    use std::io::Write;
    let bytes = render(tool, output);
    if bytes.is_empty() {
        return 1;
    }
    // Synchronous by design: this is the terminal handoff `ui.rs` already
    // documents as the stdio exception, not IO riabuild performs.
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    if handle.write_all(&bytes).is_err() || handle.flush().is_err() {
        return 1;
    }
    0
}

async fn pass_through(
    tool: Tool,
    args: &[String],
    bin_dir: &Path,
    runner: &Arc<dyn CommandRunner>,
) -> i32 {
    let path = std::env::var("PATH").unwrap_or_default();
    let stripped = path_without(&path, bin_dir);

    let borrowed: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
    let options = RunOptions {
        env: vec![("PATH".into(), stripped)],
        ..Default::default()
    };

    runner
        .run_interactive(tool.name(), &borrowed, &options)
        .await
        // The shell's own "command not found" code rather than a riabuild
        // error, because the caller is expecting the real tool.
        .unwrap_or(127)
}

/// The only place a shim diagnostic can survive.
///
/// Claude Code runs its probe with `2>/dev/null`, so stderr is discarded on the
/// path that matters most.
fn log(message: &str) {
    if let Ok(path) = std::env::var(crate::channel::LOG_ENV) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "shim: {message}");
        }
    }
    eprintln!("riabuild: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::mime::{PNG, TEXT};

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

    #[test]
    fn xclip_renders_targets_one_per_line() {
        let out = render(
            Tool::Xclip,
            &Output::Targets(vec!["image/png".into(), "text/html".into()]),
        );
        assert_eq!(out, b"image/png\ntext/html\n");
    }

    /// The channel speaks MIME; xclip's callers expect atoms. TARGETS output
    /// that says `text/plain;charset=utf-8` is not what an xclip caller greps
    /// for.
    #[test]
    fn xclip_renders_text_targets_as_x11_atoms() {
        let out = render(Tool::Xclip, &Output::Targets(vec![TEXT.into()]));
        assert_eq!(out, b"UTF8_STRING\n");
    }

    #[test]
    fn wl_paste_renders_targets_in_its_own_vocabulary() {
        let out = render(
            Tool::WlPaste,
            &Output::Targets(vec![TEXT.into(), PNG.into()]),
        );
        assert_eq!(out, b"text/plain;charset=utf-8\nimage/png\n");
    }

    #[test]
    fn bytes_are_rendered_untouched() {
        let out = render(Tool::Xclip, &Output::Bytes(vec![0x89, b'P', b'N', b'G']));
        assert_eq!(out, vec![0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn nothing_renders_as_nothing() {
        assert!(render(Tool::Xclip, &Output::Nothing).is_empty());
    }

    /// A type the far side cannot name is dropped rather than printed raw: a
    /// caller that greps the list would otherwise match a name it cannot then
    /// request.
    #[test]
    fn a_target_with_no_name_in_this_vocabulary_is_dropped() {
        let out = render(
            Tool::Xclip,
            &Output::Targets(vec!["application/pdf".into(), PNG.into()]),
        );
        assert_eq!(out, b"image/png\n");
    }

    /// The down-and-empty contract: no channel configured produces exactly what
    /// an empty clipboard produces.
    #[tokio::test]
    async fn a_missing_channel_exits_one_with_no_output() {
        use crate::runner::FakeRunner;
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new());
        let args: Vec<String> = ["-selection", "clipboard", "-t", "TARGETS", "-o"]
            .iter()
            .map(|a| a.to_string())
            .collect();

        let code = run(
            Tool::Xclip,
            &args,
            None,
            Path::new("/home/ada/.riabuild/bin"),
            &runner,
        )
        .await;
        assert_eq!(code, 1);
    }

    /// PRIMARY is not bridged, and must not reach the channel at all.
    #[tokio::test]
    async fn the_primary_selection_exits_one_without_asking_the_laptop() {
        use crate::runner::FakeRunner;
        let fake = Arc::new(FakeRunner::new());
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let args = vec!["-o".to_string()];

        let code = run(
            Tool::Xclip,
            &args,
            Some(PathBuf::from("/nonexistent.sock")),
            Path::new("/home/ada/.riabuild/bin"),
            &runner,
        )
        .await;
        assert_eq!(code, 1);
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    /// A write is handed to the real binary, with our own directory off PATH.
    #[tokio::test]
    async fn a_write_runs_the_real_tool() {
        use crate::runner::FakeRunner;
        let fake = Arc::new(FakeRunner::new().with("xclip -selection clipboard -i", 0, "", ""));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let args: Vec<String> = ["-selection", "clipboard", "-i"]
            .iter()
            .map(|a| a.to_string())
            .collect();

        let code = run(
            Tool::Xclip,
            &args,
            None,
            Path::new("/home/ada/.riabuild/bin"),
            &runner,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(fake.calls(), vec!["xclip -selection clipboard -i"]);
    }
}
