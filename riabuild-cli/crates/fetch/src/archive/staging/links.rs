//! Whether a link entry may be created, judged by where it would point.
//!
//! `safe_join` next door guards where an entry is *written*; a link entry also
//! says where it points, and the two are different questions. `Entry::unpack`
//! writes a link target through with no validation at all, so two entries were
//! once enough to write anywhere the developer can — `x -> /home/ada/.ssh`
//! followed by `x/authorized_keys`, both of which `safe_join` waves through
//! because neither archived *path* leaves the target.

use crate::Failure;
use anyhow::Result;
use std::path::Path;

/// Whether a link entry may be created, judged by where it would point.
///
/// The same component walk `safe_join` does, and for the same stated reason:
/// these tarballs have already matched a published digest, so this is not what
/// stands between a developer and a hostile archive — it is here so the day one
/// is unpacked without a digest, the guarantee does not quietly rest on a check
/// somewhere else in the program.
///
/// A **symlink**'s relative target is resolved against the link's own directory
/// before it is walked, because that is what the kernel will do with it: Node's
/// tarball is full of `bin/npm -> ../lib/node_modules/npm/bin/npm-cli.js`,
/// which climbs out of `bin/` and is perfectly legitimate. Rejecting every `..`
/// outright would refuse the archive riabuild exists to install.
///
/// A **hard link**'s target is a path from the archive root rather than from
/// the entry beside it — `tar`'s own `unpack_in` reads it that way — so it is
/// walked from an empty base and any `..` at all leaves the tree.
///
/// An absolute target has nothing to resolve against either way, and is refused
/// on sight.
pub(super) fn check_link_target(
    target: &Path,
    relative: &Path,
    link: Option<&Path>,
    is_symlink: bool,
) -> Result<()> {
    let refuse = |why: String| {
        Failure::new(
            format!(
                "unpacking an archive into {} — {why}, so riabuild refused to unpack it",
                target.display()
            ),
            "Send this to your team lead — the archive riabuild downloaded is not the one it \
             expected, and nothing has been installed.",
        )
        .detail(format!("the entry is {}", relative.display()))
    };

    let Some(link) = link else {
        return Err(refuse("a link entry names no target".to_string()).into());
    };

    // Where the target is measured from: the link's own directory for a
    // symlink, the archive root for a hard link.
    let base = if is_symlink {
        relative.parent().unwrap_or(Path::new(""))
    } else {
        Path::new("")
    };
    let mut resolved: Vec<std::ffi::OsString> = base
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect();

    for component in link.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => resolved.push(part.to_os_string()),
            std::path::Component::ParentDir => {
                if resolved.pop().is_none() {
                    return Err(refuse(format!(
                        "it contains a link pointing outside it ({} -> {})",
                        relative.display(),
                        link.display()
                    ))
                    .into());
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(refuse(format!(
                    "it contains a link to an absolute path ({} -> {})",
                    relative.display(),
                    link.display()
                ))
                .into());
            }
        }
    }
    Ok(())
}
