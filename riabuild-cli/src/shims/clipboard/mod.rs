//! The `xclip` and `wl-paste` shims: argv in, intent out.
//!
//! Claude Code probes the Linux clipboard with exactly
//!
//! ```sh
//! xclip -selection clipboard -t TARGETS -o 2>/dev/null | grep -E "image/(png|jpeg|…)" \
//!   || wl-paste -l 2>/dev/null | grep -E "…"
//! ```
//!
//! so a binary named `xclip` earlier on `PATH` owns the image-paste path
//! entirely. Note the `2>/dev/null`: the shim's stderr is discarded, which is
//! why every diagnostic has to live outside the paste path — in the banner, in
//! `riabuild channel status`, and in the log.
//!
//! The shim's job is to be indistinguishable from the real tool, and to get out
//! of the way for anything it does not handle.

pub mod serve;

pub use serve::run;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Xclip,
    WlPaste,
}

impl Tool {
    pub fn from_name(name: &str) -> Option<Tool> {
        let base = std::path::Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());
        match base.as_str() {
            "xclip" => Some(Tool::Xclip),
            "wl-paste" => Some(Tool::WlPaste),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Tool::Xclip => "xclip",
            Tool::WlPaste => "wl-paste",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// List the available types.
    Targets,
    /// Read one type, or the preferred one when none was named.
    Read(Option<String>),
    /// A selection riabuild deliberately does not bridge. Behaves as an empty
    /// clipboard, which is what the real tool does when nothing is selected.
    Empty,
    /// Not ours. Run the real binary.
    PassThrough,
}

fn is_clipboard_selection(value: &str) -> bool {
    // xclip accepts any unambiguous prefix of `clipboard`.
    let value = value.to_ascii_lowercase();
    !value.is_empty() && "clipboard".starts_with(&value)
}

pub fn parse(tool: Tool, args: &[String]) -> Intent {
    match tool {
        Tool::Xclip => parse_xclip(args),
        Tool::WlPaste => parse_wl_paste(args),
    }
}

fn parse_xclip(args: &[String]) -> Intent {
    let mut selection: Option<String> = None;
    let mut target: Option<String> = None;
    let mut output = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-o" | "-out" | "-output" => output = true,
            "-selection" | "-sel" => {
                index += 1;
                selection = args.get(index).cloned();
            }
            "-t" | "-target" => {
                index += 1;
                target = args.get(index).cloned();
            }
            // Display and verbosity flags do not change what is read.
            "-d" | "-display" => index += 1,
            "-quiet" | "-silent" | "-verbose" | "-noutf8" | "-r" | "-rmlastnl" | "-l" => {}
            // -i, -in, -f, -filter, -version, -h and anything unrecognised are
            // not a clipboard read.
            _ => return Intent::PassThrough,
        }
        index += 1;
    }

    if !output {
        // No -o means xclip is copying, not pasting.
        return Intent::PassThrough;
    }

    // xclip's default selection is PRIMARY, not CLIPBOARD.
    match selection {
        Some(value) if is_clipboard_selection(&value) => {}
        _ => return Intent::Empty,
    }

    match target.as_deref() {
        Some("TARGETS") => Intent::Targets,
        Some(target) => Intent::Read(Some(target.to_string())),
        None => Intent::Read(None),
    }
}

fn parse_wl_paste(args: &[String]) -> Intent {
    let mut target: Option<String> = None;
    let mut list = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-l" | "--list-types" => list = true,
            "-p" | "--primary" => return Intent::Empty,
            "-t" | "--type" => {
                index += 1;
                target = args.get(index).cloned();
            }
            "-n" | "--no-newline" => {}
            "-s" | "--seat" => index += 1,
            _ => return Intent::PassThrough,
        }
        index += 1;
    }

    if list {
        return Intent::Targets;
    }
    Intent::Read(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_argv(tool: Tool, argv: &[&str]) -> Intent {
        parse(
            tool,
            &argv.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        )
    }

    /// The exact probe Claude Code runs on Linux. If this row is wrong, nothing
    /// else in the design matters.
    #[test]
    fn the_claude_code_probe_is_a_targets_request() {
        let intent = parse_argv(
            Tool::Xclip,
            &["-selection", "clipboard", "-t", "TARGETS", "-o"],
        );
        assert_eq!(intent, Intent::Targets);
        assert_eq!(parse_argv(Tool::WlPaste, &["-l"]), Intent::Targets);
        assert_eq!(
            parse_argv(Tool::WlPaste, &["--list-types"]),
            Intent::Targets
        );
    }

    #[test]
    fn a_typed_read_carries_its_type() {
        assert_eq!(
            parse_argv(
                Tool::Xclip,
                &["-selection", "clipboard", "-t", "image/png", "-o"]
            ),
            Intent::Read(Some("image/png".into()))
        );
        assert_eq!(
            parse_argv(Tool::WlPaste, &["-t", "image/png"]),
            Intent::Read(Some("image/png".into()))
        );
        assert_eq!(
            parse_argv(Tool::WlPaste, &["--type", "text/html"]),
            Intent::Read(Some("text/html".into()))
        );
    }

    /// No type requested: serve the first type in preference order.
    #[test]
    fn a_read_with_no_type_asks_for_the_preferred_one() {
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "clipboard", "-o"]),
            Intent::Read(None)
        );
        assert_eq!(parse_argv(Tool::WlPaste, &[]), Intent::Read(None));
        assert_eq!(parse_argv(Tool::WlPaste, &["-n"]), Intent::Read(None));
    }

    /// xclip's default selection is PRIMARY, not CLIPBOARD. PRIMARY is the X11
    /// highlight buffer, it changes on every mouse drag, and bridging it is a
    /// firehose for no benefit — so it is empty rather than wrong.
    #[test]
    fn xclip_without_a_selection_is_primary_and_is_not_bridged() {
        assert_eq!(parse_argv(Tool::Xclip, &["-o"]), Intent::Empty);
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "primary", "-o"]),
            Intent::Empty
        );
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "secondary", "-o"]),
            Intent::Empty
        );
    }

    #[test]
    fn wl_paste_can_be_asked_for_the_primary_selection_too() {
        assert_eq!(parse_argv(Tool::WlPaste, &["-p"]), Intent::Empty);
        assert_eq!(parse_argv(Tool::WlPaste, &["--primary"]), Intent::Empty);
    }

    /// xclip abbreviates. `-sel c`, `-selection clip` and `-sel clipboard` are
    /// all the clipboard, and a developer's muscle memory uses all of them.
    #[test]
    fn the_clipboard_selection_is_recognised_however_it_is_abbreviated() {
        for selection in ["c", "clip", "clipboard", "CLIPBOARD"] {
            assert_eq!(
                parse_argv(Tool::Xclip, &["-selection", selection, "-o"]),
                Intent::Read(None),
                "-selection {selection}"
            );
            assert_eq!(
                parse_argv(Tool::Xclip, &["-sel", selection, "-o"]),
                Intent::Read(None),
                "-sel {selection}"
            );
        }
    }

    /// Anything that writes is not ours. The channel is read-only, and a write
    /// that silently did nothing would be worse than one that works locally.
    #[test]
    fn writes_are_passed_through_to_the_real_binary() {
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "clipboard", "-i"]),
            Intent::PassThrough
        );
        // xclip with no -o at all reads stdin and copies.
        assert_eq!(
            parse_argv(Tool::Xclip, &["-selection", "clipboard"]),
            Intent::PassThrough
        );
        assert_eq!(
            parse_argv(Tool::WlPaste, &["--watch", "cat"]),
            Intent::PassThrough
        );
    }

    #[test]
    fn informational_flags_are_passed_through() {
        for argv in [vec!["-version"], vec!["-h"], vec!["--help"]] {
            assert_eq!(
                parse_argv(Tool::Xclip, &argv),
                Intent::PassThrough,
                "{argv:?}"
            );
        }
        for argv in [vec!["--version"], vec!["-h"]] {
            assert_eq!(
                parse_argv(Tool::WlPaste, &argv),
                Intent::PassThrough,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn only_the_two_shimmed_tools_are_recognised() {
        assert_eq!(Tool::from_name("xclip"), Some(Tool::Xclip));
        assert_eq!(Tool::from_name("wl-paste"), Some(Tool::WlPaste));
        assert_eq!(Tool::from_name("/usr/bin/xclip"), Some(Tool::Xclip));
        assert_eq!(Tool::from_name("pbpaste"), None);
        // wl-copy writes, and the channel is read-only.
        assert_eq!(Tool::from_name("wl-copy"), None);
    }
}
