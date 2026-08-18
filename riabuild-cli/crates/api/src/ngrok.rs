//! The team's ngrok authtoken.
//!
//! Fetched by `riabuild internal ngrok-token`, which the generated `ngrok` shim
//! runs on every invocation. The token is printed on stdout, captured by a
//! command substitution, and handed to ngrok in its environment — it reaches no
//! argument list, because `ps` is world-readable on a shared server, and no
//! file, because riabuild does not write this class of secret down.
//!
//! Unlike an Infisical credential it is long-lived and one org shares it, so
//! the server writes an audit row for every fetch. That row is the only
//! attribution there is: ngrok's own dashboard sees one account.

use crate::ApiClient;
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NgrokAuthToken {
    pub token: String,
}

pub async fn fetch_authtoken(api: &ApiClient) -> Result<NgrokAuthToken> {
    api.get_json("/api/v1/org/ngrok-token").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_read_from_the_reply() {
        let fetched: NgrokAuthToken =
            serde_json::from_str(r#"{"token":"2abcDEF_ngrok_authtoken","updatedAt":1755000000}"#)
                .unwrap();
        assert_eq!(fetched.token, "2abcDEF_ngrok_authtoken");
    }

    #[test]
    fn a_reply_carrying_no_token_is_not_read_as_an_empty_one() {
        // An empty `NGROK_AUTHTOKEN` in the shim's environment looks to ngrok
        // exactly like no token at all, and the developer would be told to
        // check their team lead's settings when the real fault was here.
        serde_json::from_str::<NgrokAuthToken>(r#"{"updatedAt":1755000000}"#)
            .expect_err("a reply without a token must not decode");
    }
}
