//! Deriving an SSH public key from a private one, with no key mathematics.
//!
//! The port of `riabuild-web/convex/lib/opensshKey.ts`, field for field and
//! message for message. Both exist because the derivation happens twice on
//! purpose: riabuild-web derives what it stores, and this crate re-derives it
//! to check that a row's public and private halves are still the same key pair
//! before riabuild offers that key to a server. Two implementations of one
//! format is a real cost, and it is paid so that the check is not simply the
//! server marking its own homework.
//!
//! They are kept honest by sharing fixtures: the constants in this file's tests
//! are copied from `convex/lib/opensshKey.fixtures.ts`, so a change to either
//! parser that moved a byte would fail on one side.
//!
//! An OpenSSH private key file *contains* its own public key, in the clear, as
//! a length-prefixed field before the encrypted section. After base64-decoding
//! the body between the PEM markers:
//!
//! ```text
//! "openssh-key-v1\0"
//! string  ciphername      "none" when the key has no passphrase
//! string  kdfname
//! string  kdfoptions
//! uint32  number of keys
//! string  publickey       <-- what this module is for
//! string  encrypted section
//! ```
//!
//! Every one of those lengths comes from a row a human typed into a dashboard,
//! so [`Reader`] bounds-checks each against the buffer. A parser that trusted
//! them would panic — taking a developer's whole run down over one bad row —
//! which is the failure this module most has to avoid, because "not an SSH key
//! at all" is by far the likeliest thing to arrive here.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const MAGIC: &[u8] = b"openssh-key-v1";
const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";

/// A key's public half, read out of its private half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicHalf {
    /// `ssh-ed25519`, `ssh-rsa`, `ecdsa-sha2-nistp256`, …
    pub key_type: String,
    /// An ordinary `authorized_keys` line: `<key_type> <base64 blob>`, no comment.
    pub public_key: String,
    /// `SHA256:…`, unpadded — what `ssh-keygen -lf` prints.
    pub fingerprint: String,
}

/// A cursor over the length-prefixed fields OpenSSH serialises with.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, offset: 0 }
    }

    fn uint32(&mut self) -> Result<u32, String> {
        let end = self
            .offset
            .checked_add(4)
            .ok_or_else(|| truncated().to_string())?;
        let slice = self.bytes.get(self.offset..end).ok_or_else(truncated)?;
        self.offset = end;
        // The slice is exactly four bytes, so this cannot fail.
        Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn string(&mut self) -> Result<&'a [u8], String> {
        let length = self.uint32()? as usize;
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| truncated().to_string())?;
        let slice = self.bytes.get(self.offset..end).ok_or_else(truncated)?;
        self.offset = end;
        Ok(slice)
    }
}

fn truncated() -> String {
    "that key is truncated — it ends mid-field".to_string()
}

/// Reads a private key's public half out of it.
///
/// `Err` is a sentence, not a type: every caller turns it into one line of a
/// refusal list beside the label of the key it refused, the same shape
/// [`crate::remotes`] uses for a server it will not connect to.
pub fn public_half(private_key: &str) -> Result<PublicHalf, String> {
    let text = private_key.trim();
    let Some(rest) = text.strip_prefix(BEGIN) else {
        return Err(format!("that is not an OpenSSH private key (no {BEGIN})"));
    };
    let Some(body) = rest.strip_suffix(END) else {
        return Err(format!("that key is missing its {END} line"));
    };

    let packed: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD
        .decode(packed.as_bytes())
        .map_err(|_| "that key's body is not valid base64".to_string())?;

    if bytes.len() <= MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC || bytes[MAGIC.len()] != 0 {
        return Err("that key is not in the openssh-key-v1 format".to_string());
    }

    let mut reader = Reader::new(&bytes[MAGIC.len() + 1..]);
    let cipher_name = reader.string()?;
    reader.string()?; // kdfname
    reader.string()?; // kdfoptions

    if cipher_name != b"none" {
        // The same refusal riabuild-web makes at the paste box, restated here
        // because this crate must survive a riabuild-web that forgot its own —
        // and because the consequence lands *here*: `ssh-add` would prompt for
        // the passphrase mid-run, with nobody able to answer it.
        return Err("that key is protected by a passphrase, so riabuild cannot use it".to_string());
    }

    let count = reader.uint32()?;
    if count != 1 {
        return Err(format!("that key holds {count} keys rather than one"));
    }

    let blob = reader.string()?;
    // The blob is itself length-prefixed, and its first field names the
    // algorithm — the same string that opens an `authorized_keys` line.
    let key_type = Reader::new(blob).string()?;
    let key_type = std::str::from_utf8(key_type)
        .map_err(|_| "that key's type is not readable text".to_string())?;
    if key_type.len() < 4
        || key_type.len() > 64
        || !key_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '@'))
    {
        return Err("that key does not name a key type riabuild recognises".to_string());
    }

    let digest = ring::digest::digest(&ring::digest::SHA256, blob);
    let fingerprint = format!(
        "SHA256:{}",
        STANDARD.encode(digest.as_ref()).trim_end_matches('=')
    );

    Ok(PublicHalf {
        key_type: key_type.to_string(),
        public_key: format!("{key_type} {}", STANDARD.encode(blob)),
        fingerprint,
    })
}

/// The same keys `convex/lib/opensshKey.fixtures.ts` holds, copied across.
///
/// Two parsers of one format have to be shown to agree, and shared expected
/// values are what shows it: every constant below was read back from
/// `ssh-keygen` itself, so a change on either side that moved a byte fails
/// here or there rather than in production. Throwaway key pairs that open
/// nothing.
#[cfg(test)]
pub(crate) mod fixtures {
    pub const ED25519_PRIVATE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDwOPf38A2IAPJ0VjY2A7V8K7450q9XpAAzmfBt6INNZAAAAJAhhiUXIYYl
FwAAAAtzc2gtZWQyNTUxOQAAACDwOPf38A2IAPJ0VjY2A7V8K7450q9XpAAzmfBt6INNZA
AAAEDqLqalwICHD7Bc12lhEHodOhE1jDxTZ6PNC3HnIONmB/A49/fwDYgA8nRWNjYDtXwr
vjnSr1ekADOZ8G3og01kAAAAB2ZpeHR1cmUBAgMEBQY=
-----END OPENSSH PRIVATE KEY-----
";

    /// `fx_ed.pub` without its trailing ` fixture` comment — free text nobody
    /// chose, which would otherwise become part of the value being compared.
    pub const ED25519_PUBLIC: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPA49/fwDYgA8nRWNjYDtXwrvjnSr1ekADOZ8G3og01k";

    pub const ED25519_FINGERPRINT: &str = "SHA256:X4Nt8DcFy4DCOoCxomm4oJjRFs6sQN36IJHq7jWTD9E";

    pub const RSA_PRIVATE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEAomMEuy+oF5IbHjUP5JbVKdZSZMaJjsW15+vSZ6DrvNXlzBFDUOHM
eF917n4Bb9O5CvF8znbCIQrCcugl/3xzE0gljuNPgm7iE/XiSAZ+9h3lSEvzkR67cGbc9G
eo7grXAX31uVf50NrZs2WIydGTFoZq5oSZf3BEMAq9Pp7lajMppIwI7KsgAWt2B9PLgByK
ay0nBaDLTugOM5e7HzHwEg+Xv/2osQYQCwOMFvwMibbpOre4IZiNlp0u0mTKlIzOsftzTZ
IW4LjaMXByB53UoJgpyzUBZw1+RkGcWi0u0fxYJ0X2iQ9sqO5jWxgNgCYX/JVtWm6773VZ
1T7XzIF9IQAAA8DNg3pYzYN6WAAAAAdzc2gtcnNhAAABAQCiYwS7L6gXkhseNQ/kltUp1l
JkxomOxbXn69JnoOu81eXMEUNQ4cx4X3XufgFv07kK8XzOdsIhCsJy6CX/fHMTSCWO40+C
buIT9eJIBn72HeVIS/ORHrtwZtz0Z6juCtcBffW5V/nQ2tmzZYjJ0ZMWhmrmhJl/cEQwCr
0+nuVqMymkjAjsqyABa3YH08uAHIprLScFoMtO6A4zl7sfMfASD5e//aixBhALA4wW/AyJ
tuk6t7ghmI2WnS7SZMqUjM6x+3NNkhbguNoxcHIHndSgmCnLNQFnDX5GQZxaLS7R/FgnRf
aJD2yo7mNbGA2AJhf8lW1abrvvdVnVPtfMgX0hAAAAAwEAAQAAAQAXktxa+D4kvdcl+XoH
K0ZivnRToObTTSxtMLToylmunjav+0mUclMmnmUWbEB1JX1Vc10089SWy2MTH1R01HI4OF
8LcUBXpRU45Jcm8Zp4zDo+1pfTV2zKkoQ9DtddR0GTO9/yOi1P/pVgD7td4QjDWlwmftVx
xLCBcO2sK5EOa4e9ZGK8S7Hz01Gxy6Mqqtuow0NrQhJ591tQz9rNkXQiyf8AdEHPUPU+mc
nRoRTPYMRgVh4ZV8GS5g+2I57MvCuBiNZ2AFqz6lWdU9cwvOSBrWHeg9qIrWw65J3EKHuu
PZrUJ03IFb5Do0dJwN4U+6pTAFSZXC1oec1+gH3VK3MpAAAAgDpafN9B9ENzJJ5lU1by11
5je4T5JRNa4/lUO3GjFfcR4zyKwf+bfeQZJrJ3iHQTJEXwC07fEfOHlglYMjv8sOJSyp+2
332Auwsb7PY/VFycz9FQSUOrW6U4BCf/SNi0y2a+H3ORIH3O0TBfti6hGhGOq0gdVtbyaZ
usraDqdhy5AAAAgQDU0/z1f5YfschtUURNYsU97uu05ZZUzDZbbRq6oksLmudUfPUPLRho
1cNe5tcK0TtqxhFmdbkEiNk/YDpY8nyA6BKOVSo9b3+P8/njMn+NVEpT2u8C1vz1xeMJxM
JiPcxardEG4avxi7OKtF/kT2elzOplb7GvGoGcCmDonv+AHwAAAIEAw1OmX+EmfAuwBDdp
om6MPo6whBXiKY2tTgdAJaLVu7h8U/yAYr4izsYzoFCf9pGICiF91o5i38lgMuzUQpa8gb
qCYErvxKmKZwpaZgZbbuQRCVXG2qdQY0Cu2PQSYn06PXetY+lhDMFhFYrkgl3ZGnL5PEml
44mvgRSYLjRqWr8AAAAHZml4dHVyZQECAwQ=
-----END OPENSSH PRIVATE KEY-----
";

    pub const RSA_PUBLIC: &str = concat!(
        "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCiYwS7L6gXkhseNQ/kltUp1lJkxomOxbXn69Jno",
        "Ou81eXMEUNQ4cx4X3XufgFv07kK8XzOdsIhCsJy6CX/fHMTSCWO40+CbuIT9eJIBn72HeVIS/ORH",
        "rtwZtz0Z6juCtcBffW5V/nQ2tmzZYjJ0ZMWhmrmhJl/cEQwCr0+nuVqMymkjAjsqyABa3YH08uAH",
        "IprLScFoMtO6A4zl7sfMfASD5e//aixBhALA4wW/AyJtuk6t7ghmI2WnS7SZMqUjM6x+3NNkhbgu",
        "NoxcHIHndSgmCnLNQFnDX5GQZxaLS7R/FgnRfaJD2yo7mNbGA2AJhf8lW1abrvvdVnVPtfMgX0h"
    );

    pub const RSA_FINGERPRINT: &str = "SHA256:MgxOF2TqJxTgu35QWHCJUOETjhUKOTGIgtmxvr0q+Hs";

    /// `ssh-keygen -t ed25519 -N hunter2`. Its `ciphername` is `aes256-ctr`.
    pub const ENCRYPTED_PRIVATE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABCYeuXZ03
R28JrAZMCfQmRaAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIGcDbMboHmvPM1IT
Pfz18AMrYOEeZJhEEu+4HcNnBh7pAAAAkL1I5aqzAPscEKp0mUmrjcL8xkuPX6wYhr169G
jpzeE69JnokX9DkJFAL56Q/jlmfhIXf8R8wkGarTDnQOo09veuR742Ic9EyYfCMUfflK7d
68ApQos3tzXXugFbGEsi8NAZY7264YEDyYiZNVEgcLUbpoq4Gx2FoqXpAk0bywk+aiugul
Gn3tFGN8mMNxglXQ==
-----END OPENSSH PRIVATE KEY-----
";

    /// A *different* key's public half — for the one test that matters most
    /// here: a row whose two halves are not the same key pair.
    pub const OTHER_PUBLIC: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPphI59nx1X/yP8/S7vZh9OrQ0JejkDp2YET7IoQTjJE";
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixtures::{
        ED25519_FINGERPRINT, ED25519_PRIVATE, ED25519_PUBLIC, ENCRYPTED_PRIVATE, RSA_FINGERPRINT,
        RSA_PRIVATE, RSA_PUBLIC,
    };

    #[test]
    fn the_public_half_comes_out_of_the_private_key_itself() {
        let half = public_half(ED25519_PRIVATE).expect("parses");
        assert_eq!(half.key_type, "ssh-ed25519");
        assert_eq!(half.public_key, ED25519_PUBLIC);
        assert_eq!(half.fingerprint, ED25519_FINGERPRINT);
    }

    #[test]
    fn an_rsa_key_parses_the_same_way() {
        // Nothing here is per-algorithm: the public blob is a field in the
        // container, not something computed from the private scalar. This is
        // the test that keeps it that way.
        let half = public_half(RSA_PRIVATE).expect("parses");
        assert_eq!(half.key_type, "ssh-rsa");
        assert_eq!(half.public_key, RSA_PUBLIC);
        assert_eq!(half.fingerprint, RSA_FINGERPRINT);
    }

    #[test]
    fn a_passphrase_protected_key_is_refused() {
        let refused = public_half(ENCRYPTED_PRIVATE).expect_err("must not parse");
        assert!(refused.contains("passphrase"), "{refused}");
    }

    #[test]
    fn junk_is_refused_rather_than_panicking() {
        // Each of these reaches a length prefix that would index past the end
        // of the buffer. A panic here would take down a developer's run over a
        // row somebody typed wrong in a browser.
        for junk in [
            "",
            "hello",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nbm90LWEta2V5\n-----END OPENSSH PRIVATE KEY-----",
            "-----BEGIN OPENSSH PRIVATE KEY-----\n!!!!\n-----END OPENSSH PRIVATE KEY-----",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n",
        ] {
            assert!(public_half(junk).is_err(), "{junk:?} must be refused");
        }
    }

    #[test]
    fn a_truncated_container_is_refused_rather_than_read_short() {
        // Keep the header and the closing line, drop the middle. Every length
        // prefix that survives now describes bytes that are not there.
        let lines: Vec<&str> = ED25519_PRIVATE.trim().lines().collect();
        let truncated = format!("{}\n{}\n{}", lines[0], lines[1], lines[lines.len() - 1]);
        assert!(public_half(&truncated).is_err());
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        // What a paste box and a JSON round trip between them leave behind.
        let padded = format!("\n  {}  \n\n", ED25519_PRIVATE.trim());
        assert_eq!(
            public_half(&padded).expect("parses").fingerprint,
            ED25519_FINGERPRINT
        );
    }
}
