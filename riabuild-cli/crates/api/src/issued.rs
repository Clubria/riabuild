//! The SSH keys the org issues to this developer.
//!
//! The shape and the reasoning follow [`crate::remotes`] exactly, including why
//! this file exists at all: riabuild-web validates on the way out and riabuild
//! validates again on the way in, so the CLI survives a server that forgets its
//! own check — the precedent `org::version_only` sets — and one unusable row
//! costs a developer that row rather than the whole list.
//!
//! What arrives here is different from an address in one way that matters. An
//! address is inert until `ssh` is handed it; a private key is a credential the
//! moment it exists. So there is one check here with no analogue over there:
//!
//! **the private key's own embedded public half must equal the `publicKey` the
//! server sent.**
//!
//! That is what [`crate::openssh`] is for, and it is why the derivation is done
//! twice. Without it, riabuild-web deriving the fields it serves would be the
//! server marking its own homework: a row edited so that its two halves
//! disagree would still be offered to a server, under a fingerprint no lead
//! ever confirmed. With it, riabuild refuses that key and says so.

use crate::ApiClient;
use crate::openssh::{canonical, public_half};
use anyhow::Result;
use serde::Deserialize;

/// One key the org has issued to this developer.
///
/// `id` is the riabuild-web row id, and is what names the public-key file the
/// agent addresses this identity by — so it has to stay stable across a rename
/// and across a `replaceKey`, which is what a row id is and what a label is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedKey {
    pub id: String,
    pub label: String,
    pub key_type: String,
    pub public_key: String,
    pub fingerprint: String,
    pub private_key: String,
}

/// What one fetch produced: the keys this laptop will use, and one sentence per
/// key it refused.
///
/// Two fields rather than a `Vec` and a silent filter, for the reason
/// `remotes::Fetched` gives — a credential that has quietly vanished is a
/// support ticket, and the reason it went is the whole content of the answer.
#[derive(Debug, Clone, Default)]
pub struct Fetched {
    pub keys: Vec<IssuedKey>,
    pub refused: Vec<String>,
}

/// The wire shape. `#[serde(default)]` on every field for the same reason
/// `remotes::WireServer` carries it: serde fails the *whole reply* on a field
/// it cannot read, so one malformed row would empty a developer's key ring
/// rather than dropping itself.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WireKey {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    // There is deliberately no `key_type` here, though the endpoint sends one.
    // The type riabuild reports is read out of the key itself, so a field for
    // the server's answer would be a second source for one fact — and the one
    // nothing has checked. serde ignores what it is not asked for.
    #[serde(default)]
    public_key: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    private_key: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Reply {
    #[serde(default)]
    keys: Vec<WireKey>,
}

/// One key, checked. `Err` is the sentence the developer is shown.
fn usable(wire: &WireKey) -> Result<IssuedKey, String> {
    let label = wire.label.trim();
    let id = wire.id.trim();

    // Charset-checked, not merely non-empty, and this is the one field where
    // that is load-bearing rather than tidy: `id` is what
    // `remote::issued::agent` interpolates into `self.dir.join(format!("{id}.pub"))`,
    // so it is the only value in this reply that becomes a *path*. An allowlist
    // rather than a blocklist, for the reason `askpass::hash_of` gives about
    // the same class of value — a `/` or a `..` in a server-supplied id would
    // otherwise put a file wherever the server liked, and a leading `-` would
    // reach `ssh-add`'s argv as an option. 64 rather than the label's 32
    // because a row id is riabuild-web's to choose, not a person's.
    // The leading-character rule is the one thing here the label check does
    // not need: `.` is a perfectly good character inside an id and a fatal one
    // in front of it (`..` is the parent directory, `.foo` is a dotfile), and
    // a leading `-` is an option rather than a name wherever the path is
    // passed as an argument. `remotes::usable` refuses a host beginning `-`
    // for the second half of that reason.
    if id.is_empty()
        || id.len() > 64
        || !id.chars().all(is_label_char)
        || id.starts_with('.')
        || id.starts_with('-')
    {
        return Err(format!("{label:?} arrived without a usable id"));
    }
    if label.is_empty() || label.len() > 32 || !label.chars().all(is_label_char) {
        return Err(format!("{label:?} is not a usable key name"));
    }
    if wire.private_key.trim().is_empty() {
        return Err(format!("{label:?} arrived without a private key"));
    }

    // The check with no analogue in `remotes`. If these disagree, something has
    // edited the row's fields apart from each other, and riabuild must not
    // offer a server a credential whose fingerprint it cannot vouch for.
    let derived = public_half(&wire.private_key).map_err(|why| format!("{label:?}: {why}"))?;
    if derived.public_key != wire.public_key.trim() {
        return Err(format!(
            "{label:?} does not match its own public key — the row says {}, the key itself says {}",
            short(wire.fingerprint.trim()),
            short(&derived.fingerprint)
        ));
    }

    Ok(IssuedKey {
        id: id.to_string(),
        label: label.to_string(),
        // Both taken from the key rather than from the reply, now that they are
        // known to agree: what riabuild reports is then what riabuild parsed,
        // and cannot drift from it in a later edit to this function.
        key_type: derived.key_type,
        public_key: derived.public_key,
        fingerprint: derived.fingerprint,
        // Re-emitted, never passed through. `public_half` had to normalise the
        // framing away to read this key at all, and treating that as a claim
        // that the original bytes were usable is what shipped broken: a key
        // with CRLF endings, no trailing newline, an unwrapped body or indented
        // lines validates here and is then refused by `ssh-add` with
        // `error in libcrypto`. See `openssh::canonical`.
        private_key: canonical(&wire.private_key).map_err(|why| format!("{label:?}: {why}"))?,
    })
}

/// Enough of a fingerprint to tell two apart, in a sentence that has to fit on
/// a terminal line beside another one.
fn short(fingerprint: &str) -> String {
    if fingerprint.is_empty() {
        return "nothing".to_string();
    }
    fingerprint.chars().take(20).collect::<String>() + "…"
}

fn is_label_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

/// `GET /api/v1/issued-keys`.
///
/// Fails only when the request or the reply's *envelope* did; a key inside it
/// that this riabuild will not use is refused individually.
pub async fn fetch_issued(api: &ApiClient) -> Result<Fetched> {
    let reply: Reply = api.get_json("/api/v1/issued-keys").await?;
    Ok(sort_out(reply.keys))
}

fn sort_out(wire: Vec<WireKey>) -> Fetched {
    let mut fetched = Fetched::default();
    for key in wire {
        match usable(&key) {
            Ok(key) => fetched.keys.push(key),
            Err(reason) => fetched.refused.push(reason),
        }
    }
    fetched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openssh::fixtures::{
        ED25519_FINGERPRINT, ED25519_PRIVATE, ED25519_PUBLIC, ENCRYPTED_PRIVATE, OTHER_PUBLIC,
        RSA_PRIVATE, RSA_PUBLIC,
    };

    fn wire() -> WireKey {
        WireKey {
            id: "k17abc".into(),
            label: "prod-bastion".into(),
            public_key: ED25519_PUBLIC.into(),
            fingerprint: ED25519_FINGERPRINT.into(),
            private_key: ED25519_PRIVATE.into(),
        }
    }

    #[test]
    fn an_ordinary_key_is_accepted_whole() {
        let key = usable(&wire()).expect("usable");
        assert_eq!(key.id, "k17abc");
        assert_eq!(key.label, "prod-bastion");
        assert_eq!(key.key_type, "ssh-ed25519");
        assert_eq!(key.public_key, ED25519_PUBLIC);
        assert_eq!(key.fingerprint, ED25519_FINGERPRINT);
        // Canonical, not the wire string — see `openssh::canonical`. Equal
        // here only because the fixture is already what ssh-keygen writes.
        assert_eq!(key.private_key, ED25519_PRIVATE);
    }

    #[test]
    fn a_key_the_row_stored_with_broken_framing_is_repaired_rather_than_passed_on() {
        // The bug a developer hit against a real server: riabuild validated the
        // key, then handed `ssh-add` the bytes it had been given, and OpenSSH
        // refused them with `error in libcrypto`. Both halves matter here — the
        // row must still be accepted (the key is fine), and what comes out must
        // be what `ssh-keygen` would have written.
        let mangled = ED25519_PRIVATE.trim_end().replace('\n', "\r\n");
        let key = usable(&WireKey {
            private_key: mangled,
            ..wire()
        })
        .expect("a key with odd framing is still a usable key");

        assert_eq!(key.private_key, ED25519_PRIVATE);
        assert!(
            key.private_key
                .ends_with("-----END OPENSSH PRIVATE KEY-----\n")
        );
        assert!(!key.private_key.contains('\r'));
    }

    #[test]
    fn a_key_whose_halves_disagree_is_refused() {
        // The check this module exists for. A row edited so that its public and
        // private halves are different key pairs would otherwise be offered to
        // a server under a fingerprint no lead ever confirmed.
        let refused = usable(&WireKey {
            public_key: OTHER_PUBLIC.into(),
            ..wire()
        })
        .expect_err("must not be usable");
        assert!(refused.contains("does not match"), "{refused}");
        // And it names both halves, because "does not match" alone leaves a
        // lead with nowhere to look.
        assert!(refused.contains("SHA256:"), "{refused}");
    }

    #[test]
    fn the_reported_type_and_fingerprint_come_from_the_key_not_the_reply() {
        // riabuild-web could serve a fingerprint that is merely stale. What the
        // terminal names has to be what riabuild actually loaded into an agent.
        let key = usable(&WireKey {
            fingerprint: "SHA256:whatever-the-row-happens-to-say".into(),
            ..wire()
        })
        .expect("usable");
        assert_eq!(key.key_type, "ssh-ed25519");
        assert_eq!(key.fingerprint, ED25519_FINGERPRINT);
    }

    #[test]
    fn a_passphrase_protected_key_is_refused_here_too() {
        // riabuild-web refuses these at the paste box. This is the client
        // surviving a riabuild-web that forgot to — and the consequence lands
        // on this side: `ssh-add` would sit on a prompt nobody can answer.
        let refused = usable(&WireKey {
            private_key: ENCRYPTED_PRIVATE.into(),
            ..wire()
        })
        .expect_err("must not be usable");
        assert!(refused.contains("passphrase"), "{refused}");
    }

    #[test]
    fn a_key_without_an_id_or_a_usable_name_is_refused() {
        for bad in [
            WireKey {
                id: "  ".into(),
                ..wire()
            },
            WireKey {
                label: "has space".into(),
                ..wire()
            },
            WireKey {
                label: "a".repeat(33),
                ..wire()
            },
            WireKey {
                label: String::new(),
                ..wire()
            },
            WireKey {
                private_key: "   ".into(),
                ..wire()
            },
        ] {
            assert!(usable(&bad).is_err(), "{:?} must not be usable", bad.label);
        }
    }

    #[test]
    fn an_id_that_would_choose_where_a_file_lands_is_refused() {
        // `id` is the one field in this reply that becomes a path:
        // `remote::issued::agent::Agent::add` writes `<agent dir>/<id>.pub`.
        // A server that sent `../../../.ssh/authorized_keys` would otherwise
        // be choosing which file riabuild writes a public key into, and a
        // leading `-` would reach an argv as an option.
        for bad in [
            "../../../.ssh/authorized_keys",
            "..",
            "a/b",
            "a\\b",
            "-oProxyCommand=id",
            "k17 abc",
            "k17\nabc",
            "k17;rm",
            "$(id)",
            "\u{2044}",
        ] {
            let refused = usable(&WireKey {
                id: bad.into(),
                ..wire()
            });
            assert!(
                refused.is_err(),
                "an id of {bad:?} must not be usable: {:?}",
                refused.map(|key| key.id)
            );
        }
        // And the length bound, so a row id cannot become a path component of
        // any size the server likes.
        assert!(
            usable(&WireKey {
                id: "k".repeat(65),
                ..wire()
            })
            .is_err()
        );
        // The ordinary shapes riabuild-web actually issues still pass.
        for good in ["k17abc", "j5-7_x.2", &"k".repeat(64)] {
            assert!(
                usable(&WireKey {
                    id: good.into(),
                    ..wire()
                })
                .is_ok(),
                "{good:?} is an ordinary row id"
            );
        }
    }

    #[test]
    fn one_refused_key_does_not_cost_the_developer_the_rest() {
        let fetched = sort_out(vec![
            wire(),
            WireKey {
                label: "broken".into(),
                public_key: OTHER_PUBLIC.into(),
                ..wire()
            },
            WireKey {
                id: "k2".into(),
                label: "gpu-box".into(),
                public_key: RSA_PUBLIC.into(),
                fingerprint: String::new(),
                private_key: RSA_PRIVATE.into(),
            },
        ]);
        assert_eq!(
            fetched
                .keys
                .iter()
                .map(|key| key.label.as_str())
                .collect::<Vec<_>>(),
            vec!["prod-bastion", "gpu-box"]
        );
        assert_eq!(fetched.refused.len(), 1, "{:?}", fetched.refused);
    }

    #[test]
    fn a_reply_decodes_and_an_unknown_field_in_it_is_ignored() {
        // Forward compatibility, the rule `/api/v1` is held to: a field added
        // later must not break a riabuild released before it.
        let json = format!(
            r#"{{"keys":[{{"id":"k1","label":"prod-bastion","keyType":"ssh-ed25519",
               "publicKey":{public:?},"fingerprint":{fingerprint:?},"privateKey":{private:?},
               "issuedBy":"grace"}}]}}"#,
            public = ED25519_PUBLIC,
            fingerprint = ED25519_FINGERPRINT,
            private = ED25519_PRIVATE,
        );
        let reply: Reply = serde_json::from_str(&json).expect("decodes");
        let fetched = sort_out(reply.keys);
        assert_eq!(fetched.keys.len(), 1, "{:?}", fetched.refused);
        assert_eq!(fetched.keys[0].fingerprint, ED25519_FINGERPRINT);
    }

    #[test]
    fn a_reply_with_no_keys_field_at_all_is_an_empty_list() {
        // What a riabuild-web deployed before this endpoint existed answers,
        // and it must read as "no keys issued" rather than as a broken reply.
        let reply: Reply = serde_json::from_str("{}").expect("decodes");
        assert!(sort_out(reply.keys).keys.is_empty());
    }
}
