//! The one string that connects a client to a server.
//!
//! `sbx://host:port/<token>#<fingerprint>` -- an address, a credential and the
//! certificate to expect, in something a person can paste once. Parsed here
//! rather than at each end, because a client and a server disagreeing about
//! where the fingerprint stops is a failure that looks exactly like a wrong
//! token.
//!
//! **The fingerprint is the point.** A self-signed certificate that a client
//! accepts without checking is a TLS session with no idea who it is talking to,
//! and telling people to click through a warning is how that becomes normal.
//! Carrying the fingerprint in the pairing string means the *first* connection
//! is verified too, rather than trusted and pinned afterwards, which is the
//! usual weak point of trust-on-first-use.

use std::fmt;
use std::str::FromStr;

/// Everything a client needs to reach one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// Host or address, as the *client* should dial it. Not necessarily what
    /// the server calls itself: a server inside WSL is reached at an address
    /// Windows can route to, which the server has to be told.
    pub host: String,
    pub port: u16,
    pub token: String,
    /// Lowercase hex SHA-256 of the server's certificate, DER-encoded.
    pub fingerprint: String,
}

impl Pairing {
    /// The base URL to make requests against.
    pub fn url(&self) -> String {
        format!("https://{}:{}", self.host, self.port)
    }

    /// The pairing string with the token replaced, for printing somewhere it
    /// should not be read: a log line, a `doctor` check, an error message.
    pub fn redacted(&self) -> String {
        format!(
            "sbx://{}:{}/<token>#{}",
            self.host, self.port, self.fingerprint
        )
    }
}

/// Prints in full, token included. That is what it is for, and the reason
/// [`Pairing::redacted`] exists for everywhere else.
impl fmt::Display for Pairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sbx://{}:{}/{}#{}",
            self.host, self.port, self.token, self.fingerprint
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Scheme,
    Host,
    Port,
    Token,
    Fingerprint,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            ParseError::Scheme => "a pairing string starts with `sbx://`",
            ParseError::Host => "no host in the pairing string",
            ParseError::Port => "no port, or not a number, in the pairing string",
            ParseError::Token => "no token in the pairing string",
            ParseError::Fingerprint => {
                "no `#<fingerprint>`, or not 64 hex characters, in the pairing string"
            }
        };
        f.write_str(what)
    }
}

impl std::error::Error for ParseError {}

impl FromStr for Pairing {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.trim().strip_prefix("sbx://").ok_or(ParseError::Scheme)?;
        let (authority, rest) = rest.split_once('/').ok_or(ParseError::Token)?;
        let (host, port) = authority.rsplit_once(':').ok_or(ParseError::Port)?;
        if host.is_empty() {
            return Err(ParseError::Host);
        }
        let port: u16 = port.parse().map_err(|_| ParseError::Port)?;

        let (token, fingerprint) = rest.split_once('#').ok_or(ParseError::Fingerprint)?;
        if token.is_empty() {
            return Err(ParseError::Token);
        }
        let fingerprint = fingerprint.to_ascii_lowercase();
        // Checked here rather than at the point of comparison: a truncated
        // fingerprint that is only noticed during the handshake fails as a
        // connection error, which sends you looking at the network.
        if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ParseError::Fingerprint);
        }

        Ok(Pairing {
            host: host.to_string(),
            port,
            token: token.to_string(),
            fingerprint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "3b1f0a9c4d2e8b7a6f5c4d3e2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0998";

    fn sample() -> Pairing {
        Pairing {
            host: "wsl.local".into(),
            port: 17671,
            token: "d0e1f2".into(),
            fingerprint: FP.into(),
        }
    }

    #[test]
    fn it_round_trips_through_the_string_a_person_pastes() {
        let printed = sample().to_string();
        assert_eq!(printed, format!("sbx://wsl.local:17671/d0e1f2#{FP}"));
        assert_eq!(printed.parse::<Pairing>().unwrap(), sample());
    }

    #[test]
    fn the_url_is_https_and_the_redaction_keeps_everything_but_the_token() {
        assert_eq!(sample().url(), "https://wsl.local:17671");
        let redacted = sample().redacted();
        assert!(!redacted.contains("d0e1f2"), "{redacted}");
        assert!(redacted.contains(FP), "{redacted}");
    }

    /// A fingerprint that is the wrong length is a truncated paste, and saying
    /// so beats failing during the handshake, which reads as a network fault.
    #[test]
    fn a_short_or_non_hex_fingerprint_is_refused_at_the_paste() {
        for bad in [
            format!("sbx://h:1/t#{}", &FP[..40]),
            format!("sbx://h:1/t#{}zz", &FP[..62]),
            "sbx://h:1/t".to_string(),
        ] {
            assert_eq!(
                bad.parse::<Pairing>().unwrap_err(),
                ParseError::Fingerprint,
                "{bad}"
            );
        }
    }

    #[test]
    fn the_parts_that_are_missing_are_named() {
        assert_eq!(
            format!("https://h:1/t#{FP}")
                .parse::<Pairing>()
                .unwrap_err(),
            ParseError::Scheme
        );
        assert_eq!(
            format!("sbx://:1/t#{FP}").parse::<Pairing>().unwrap_err(),
            ParseError::Host
        );
        assert_eq!(
            format!("sbx://h:nope/t#{FP}")
                .parse::<Pairing>()
                .unwrap_err(),
            ParseError::Port
        );
        assert_eq!(
            format!("sbx://h:1/#{FP}").parse::<Pairing>().unwrap_err(),
            ParseError::Token
        );
    }

    /// Whitespace is what a paste out of a terminal carries, and an uppercase
    /// fingerprint is what half the tools that print one produce.
    #[test]
    fn a_paste_survives_its_surroundings() {
        let messy = format!("  sbx://h:17671/tok#{}\n", FP.to_ascii_uppercase());
        let p: Pairing = messy.parse().unwrap();
        assert_eq!(p.fingerprint, FP, "compared lowercase, always");
    }

    /// A bracketed IPv6 literal is the form a URL uses, and the one that
    /// survives splitting an authority on its last colon.
    #[test]
    fn a_bracketed_ipv6_host_parses() {
        let p: Pairing = format!("sbx://[::1]:17671/tok#{FP}").parse().unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, 17671);
    }
}
