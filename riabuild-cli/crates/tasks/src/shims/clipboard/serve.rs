//! Running the shim: talk to the channel, print in the tool's own format, and
//! never `exec` ourselves.

use super::{Intent, Tool, parse};
use riabuild_channel::client;
use riabuild_channel::mime::{self, Vocabulary};
use riabuild_channel::protocol::{Request, Response};
use riabuild_runner::{CommandRunner, RunOptions};
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
        Tool::WlPaste | Tool::WlCopy => Vocabulary::Wayland,
    }
}

/// What a tool puts on the clipboard when the caller named no type.
///
/// Both default to plain text, and getting this wrong is silent: the content
/// arrives under a type nothing asks for, so the paste on the other side finds
/// an empty clipboard.
fn default_write_type(tool: Tool) -> &'static str {
    match tool {
        Tool::Xclip => "UTF8_STRING",
        Tool::WlPaste | Tool::WlCopy => "text/plain;charset=utf-8",
    }
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
        Intent::Write { target, literal } => {
            return write(tool, target.as_deref(), literal.as_deref(), socket).await;
        }
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

/// Sends a copy up to the laptop.
///
/// Unlike a read, this cannot degrade quietly. A read that fails looks exactly
/// like an empty clipboard, which is a state the caller already handles; a write
/// that fails and reports success loses what the developer copied. So the exit
/// status is non-zero whenever the content did not arrive — which is also what
/// the real tool does on a server with no display.
async fn write(
    tool: Tool,
    target: Option<&str>,
    literal: Option<&str>,
    socket: Option<PathBuf>,
) -> i32 {
    let native = target.unwrap_or_else(|| default_write_type(tool));
    let Some(mime) = mime::to_mime(vocabulary(tool), native) else {
        log(&format!("`{native}` is not a type the channel carries"));
        return 1;
    };

    let bytes = match literal {
        Some(text) => text.as_bytes().to_vec(),
        None => {
            use std::io::Read;
            let mut buffer = Vec::new();
            if let Err(error) = std::io::stdin().read_to_end(&mut buffer) {
                log(&format!("could not read the content to copy: {error}"));
                return 1;
            }
            buffer
        }
    };

    let Some(socket) = socket else {
        log("no clipboard channel is configured for this session");
        return 1;
    };

    let request = Request::ClipboardWrite {
        mime: mime.to_string(),
        len: bytes.len(),
    };

    match client::request_with_body(&socket, &request, &bytes).await {
        Ok(reply) => match reply.response {
            Response::Written => 0,
            other => {
                log(&describe(&other));
                1
            }
        },
        Err(error) => {
            log(&format!("{error:#}"));
            1
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
    let stripped = riabuild_paths::path_without(&path, bin_dir);

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
    if let Ok(path) = std::env::var(riabuild_channel::LOG_ENV) {
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
    use riabuild_channel::mime::{PNG, TEXT};

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
        use riabuild_runner::FakeRunner;
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
        use riabuild_runner::FakeRunner;
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

    /// A PRIMARY write is handed to the real binary, with our own directory off
    /// PATH.
    #[tokio::test]
    async fn a_primary_write_runs_the_real_tool() {
        use riabuild_runner::FakeRunner;
        let fake = Arc::new(FakeRunner::new().with("xclip -selection primary -i", 0, "", ""));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let args: Vec<String> = ["-selection", "primary", "-i"]
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
        assert_eq!(fake.calls(), vec!["xclip -selection primary -i"]);
    }

    /// A write that cannot reach the laptop must not report success: unlike a
    /// read, there is no state the caller already handles that it resembles.
    #[tokio::test]
    async fn a_write_with_no_channel_fails_rather_than_pretending() {
        use riabuild_runner::FakeRunner;
        let fake = Arc::new(FakeRunner::new());
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
        assert_eq!(code, 1);
        // And it did not quietly fall back to a tool that cannot reach the
        // laptop either.
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    /// Both tools default to plain text, and under the name their own callers
    /// use. A default of the wrong spelling is silent: the content lands under
    /// a type nothing asks for.
    #[test]
    fn the_default_write_type_is_text_in_each_vocabulary() {
        assert_eq!(
            mime::to_mime(Vocabulary::X11, default_write_type(Tool::Xclip)),
            Some(TEXT)
        );
        assert_eq!(
            mime::to_mime(Vocabulary::Wayland, default_write_type(Tool::WlCopy)),
            Some(TEXT)
        );
    }
}
