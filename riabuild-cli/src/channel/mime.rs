//! The three clipboard vocabularies, and the MIME types the channel speaks.
//!
//! macOS names pasteboard types with uniform type identifiers, X11 with
//! interned atoms that predate MIME, and Wayland with MIME strings that are
//! nearly but not quite the ones X11 uses. The agent normalises to the MIME
//! column; the shim translates back into its own tool's vocabulary. Without
//! this layer `text/html` copied out of Safari exists under no name `xclip`
//! recognises.
//!
//! The macOS column is a *laptop* platform, which is the primary case. macOS
//! as a server is out of scope.

/// UTF-8 plain text. The canonical spelling on the wire.
pub const TEXT: &str = "text/plain;charset=utf-8";
pub const HTML: &str = "text/html";
pub const PNG: &str = "image/png";
pub const TIFF: &str = "image/tiff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vocabulary {
    X11,
    Wayland,
    MacOs,
}

/// Native name → MIME. A row may hold several spellings for one type; only the
/// first is ever written back by `from_mime`.
const X11: &[(&str, &str)] = &[
    ("UTF8_STRING", TEXT),
    ("STRING", TEXT),
    ("TEXT", TEXT),
    ("text/plain;charset=utf-8", TEXT),
    ("text/plain", TEXT),
    ("text/html", HTML),
    ("image/png", PNG),
    ("image/tiff", TIFF),
];

const WAYLAND: &[(&str, &str)] = &[
    ("text/plain;charset=utf-8", TEXT),
    ("text/plain", TEXT),
    ("UTF8_STRING", TEXT),
    ("text/html", HTML),
    ("image/png", PNG),
    ("image/tiff", TIFF),
];

const MACOS: &[(&str, &str)] = &[
    ("public.utf8-plain-text", TEXT),
    ("public.plain-text", TEXT),
    ("NSStringPboardType", TEXT),
    ("public.html", HTML),
    ("public.png", PNG),
    ("public.tiff", TIFF),
];

fn table(vocab: Vocabulary) -> &'static [(&'static str, &'static str)] {
    match vocab {
        Vocabulary::X11 => X11,
        Vocabulary::Wayland => WAYLAND,
        Vocabulary::MacOs => MACOS,
    }
}

/// A native clipboard type name as this platform spells it → the MIME type the
/// channel uses. `None` for anything the channel does not carry.
pub fn to_mime(vocab: Vocabulary, native: &str) -> Option<&'static str> {
    table(vocab)
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(native))
        .map(|(_, mime)| *mime)
}

/// A MIME type → the name this platform's clipboard tool expects.
///
/// Where a platform has several spellings for one type the first row wins:
/// `UTF8_STRING` rather than the legacy `STRING`, which is what a modern
/// `xclip` wants.
pub fn from_mime(vocab: Vocabulary, mime: &str) -> Option<&'static str> {
    table(vocab)
        .iter()
        .find(|(_, m)| m.eq_ignore_ascii_case(mime))
        .map(|(name, _)| *name)
}

/// Types that name a file rather than carry one.
///
/// These are dropped at the type level and never bridged. Copying a file in
/// Finder puts `file:///Users/ada/Desktop/report.pdf` on the pasteboard;
/// carried across verbatim the server receives a path that does not exist
/// there, and it is exactly the kind of thing Claude will confidently try to
/// read.
///
/// The exclusion is type-level only. A laptop path copied as plain text is
/// byte-identical to any other string, and scanning text content for
/// path-shaped substrings would corrupt legitimate text to prevent a case the
/// developer chose deliberately.
const FILE_REFERENCE_TYPES: &[&str] = &[
    "text/uri-list",
    "public.file-url",
    "public.url",
    "x-special/gnome-copied-files",
    "x-special/nautilus-clipboard",
    "x-special/mate-copied-files",
    "application/x-kde-cutselection",
    "application/x-kde4-urilist",
    "FILE_NAME",
    "com.apple.finder.node",
    "com.apple.pasteboard.promised-file-url",
    "NSFilenamesPboardType",
];

pub fn is_file_reference(native: &str) -> bool {
    FILE_REFERENCE_TYPES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(native))
}

/// The order the channel reports types in.
///
/// This is a functional decision rather than cosmetics: callers commonly take
/// the first match, and a request with no type at all is served the first
/// entry. Text leads so that `xclip -o` on the server produces what selecting
/// the same clipboard on the laptop would.
const PREFERENCE: &[&str] = &[TEXT, HTML, PNG, TIFF];

/// What the laptop's clipboard reports → what the channel advertises.
///
/// Three rules, in order: unknown and file-reference types are dropped,
/// duplicate spellings collapse, and TIFF is omitted when PNG is present
/// because it is the same pixels at ten times the size.
pub fn normalise_targets(vocab: Vocabulary, native: &[String]) -> Vec<String> {
    let mut present: Vec<&'static str> = Vec::new();

    for name in native {
        let name = name.trim();
        if name.is_empty() || is_file_reference(name) {
            continue;
        }
        let Some(mime) = to_mime(vocab, name) else {
            continue;
        };
        if !present.contains(&mime) {
            present.push(mime);
        }
    }

    if present.contains(&PNG) {
        present.retain(|mime| *mime != TIFF);
    }

    PREFERENCE
        .iter()
        .filter(|mime| present.contains(mime))
        .map(|mime| (*mime).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is asserted in both directions for every row, because a
    /// one-way entry is exactly the bug that makes paste work on one laptop
    /// and silently fail on another.
    #[test]
    fn every_vocabulary_round_trips_through_mime() {
        let rows: &[(Vocabulary, &str, &str)] = &[
            (Vocabulary::MacOs, "public.utf8-plain-text", TEXT),
            (Vocabulary::MacOs, "public.html", HTML),
            (Vocabulary::MacOs, "public.png", PNG),
            (Vocabulary::MacOs, "public.tiff", TIFF),
            (Vocabulary::X11, "UTF8_STRING", TEXT),
            (Vocabulary::X11, "text/html", HTML),
            (Vocabulary::X11, "image/png", PNG),
            (Vocabulary::X11, "image/tiff", TIFF),
            (Vocabulary::Wayland, "text/plain;charset=utf-8", TEXT),
            (Vocabulary::Wayland, "text/html", HTML),
            (Vocabulary::Wayland, "image/png", PNG),
            (Vocabulary::Wayland, "image/tiff", TIFF),
        ];

        for (vocab, native, mime) in rows {
            assert_eq!(to_mime(*vocab, native), Some(*mime), "to_mime {native}");
            assert_eq!(from_mime(*vocab, mime), Some(*native), "from_mime {mime}");
        }
    }

    /// X11 has three names for the same UTF-8 text and only one of them is
    /// canonical. All three must read as text; only the canonical one is ever
    /// written back.
    #[test]
    fn the_legacy_x11_text_atoms_all_read_as_utf8_text() {
        for atom in ["UTF8_STRING", "STRING", "TEXT", "text/plain"] {
            assert_eq!(to_mime(Vocabulary::X11, atom), Some(TEXT), "{atom}");
        }
        assert_eq!(from_mime(Vocabulary::X11, TEXT), Some("UTF8_STRING"));
    }

    #[test]
    fn wayland_text_is_recognised_with_and_without_the_charset() {
        for native in ["text/plain;charset=utf-8", "text/plain"] {
            assert_eq!(to_mime(Vocabulary::Wayland, native), Some(TEXT), "{native}");
        }
    }

    /// An unknown type is dropped rather than guessed at. Bridging a type the
    /// far side cannot name produces a paste that fails with no explanation.
    #[test]
    fn an_unrecognised_type_has_no_mime() {
        assert_eq!(to_mime(Vocabulary::X11, "MULTIPLE"), None);
        assert_eq!(to_mime(Vocabulary::MacOs, "com.apple.webarchive"), None);
        assert_eq!(from_mime(Vocabulary::X11, "application/pdf"), None);
    }

    /// Copying a file in Finder puts a path on the pasteboard. Bridged
    /// verbatim the server receives a path that does not exist there — the one
    /// payload that is syntactically valid and semantically false on the far
    /// side.
    #[test]
    fn file_reference_types_are_recognised_in_every_vocabulary() {
        for native in [
            "text/uri-list",
            "public.file-url",
            "public.url",
            "x-special/gnome-copied-files",
            "x-special/nautilus-clipboard",
            "application/x-kde-cutselection",
            "FILE_NAME",
            "com.apple.finder.node",
        ] {
            assert!(
                is_file_reference(native),
                "{native} should be a file reference"
            );
        }
    }

    #[test]
    fn ordinary_types_are_not_file_references() {
        for native in ["image/png", "text/html", "UTF8_STRING", "public.png"] {
            assert!(!is_file_reference(native), "{native}");
        }
    }

    /// Case is not significant on the wire: X11 atoms are conventionally upper
    /// case and MIME types conventionally lower, and a laptop that reports
    /// `Image/PNG` must not silently lose its clipboard.
    #[test]
    fn lookup_ignores_case() {
        assert_eq!(to_mime(Vocabulary::X11, "image/PNG"), Some(PNG));
        assert_eq!(to_mime(Vocabulary::MacOs, "PUBLIC.PNG"), Some(PNG));
        assert!(is_file_reference("Text/URI-List"));
    }

    fn targets(vocab: Vocabulary, native: &[&str]) -> Vec<String> {
        normalise_targets(
            vocab,
            &native.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    /// macOS puts a screenshot on the pasteboard as both PNG and uncompressed
    /// TIFF, and the TIFF can be 40 MB for pixels already available losslessly.
    /// Ordering alone is insufficient — a caller that walks the whole list can
    /// still choose it — so it is omitted entirely.
    #[test]
    fn tiff_is_dropped_when_png_is_present() {
        let list = targets(Vocabulary::MacOs, &["public.tiff", "public.png"]);
        assert_eq!(list, vec![PNG]);
    }

    /// TIFF is only redundant when PNG exists. On its own it is the content.
    #[test]
    fn tiff_survives_when_it_is_the_only_image() {
        let list = targets(Vocabulary::MacOs, &["public.tiff"]);
        assert_eq!(list, vec![TIFF]);
    }

    /// The first entry is what a caller with no type preference gets, and the
    /// spec fixes that as the preferred text flavour whenever text is present.
    #[test]
    fn text_leads_when_text_is_present() {
        let list = targets(Vocabulary::X11, &["image/png", "text/html", "UTF8_STRING"]);
        assert_eq!(list, vec![TEXT, HTML, PNG]);
    }

    #[test]
    fn png_leads_when_the_clipboard_holds_only_an_image() {
        let list = targets(Vocabulary::X11, &["image/tiff", "image/png"]);
        assert_eq!(list, vec![PNG]);
    }

    /// The strong form of the rule: no input, however shaped, produces a
    /// file-reference type on the wire.
    #[test]
    fn no_file_reference_type_survives_any_input() {
        let list = targets(
            Vocabulary::X11,
            &[
                "text/uri-list",
                "x-special/gnome-copied-files",
                "FILE_NAME",
                "UTF8_STRING",
            ],
        );
        assert_eq!(list, vec![TEXT]);

        // And on its own it leaves nothing at all, rather than an empty-ish
        // list the caller would treat as a usable clipboard.
        assert!(targets(Vocabulary::MacOs, &["public.file-url"]).is_empty());
    }

    /// X11 reports three atoms for one text flavour. They must collapse, or
    /// TARGETS lists the same content three times and a caller reads it three
    /// times.
    #[test]
    fn duplicate_spellings_collapse_to_one_entry() {
        let list = targets(
            Vocabulary::X11,
            &["UTF8_STRING", "STRING", "TEXT", "text/plain"],
        );
        assert_eq!(list, vec![TEXT]);
    }

    /// X11 always reports these, and they are not content.
    #[test]
    fn unknown_and_meta_targets_are_dropped() {
        let list = targets(
            Vocabulary::X11,
            &["TARGETS", "MULTIPLE", "TIMESTAMP", "image/png"],
        );
        assert_eq!(list, vec![PNG]);
    }

    #[test]
    fn an_empty_clipboard_produces_an_empty_list() {
        assert!(targets(Vocabulary::X11, &[]).is_empty());
    }
}
