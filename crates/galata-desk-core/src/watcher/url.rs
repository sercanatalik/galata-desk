//! Where the watcher may be reached.
//!
//! The watcher's API token travels in every request. Plain HTTP is allowed
//! only where that traffic stays on this Mac, on a private network, or inside
//! Tailscale's encryption; anything else must be HTTPS. The check is on the
//! address as typed: a name is never resolved to decide, so a public name that
//! happens to resolve to a private address is still refused over HTTP.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use url::Url;

/// A watcher address that passed [`WatcherUrl::parse`]. Shown and stored as
/// typed, without a trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatcherUrl(String);

/// Why an address was refused. Each says how to fix it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WatcherUrlError {
    #[error("The address starts with http:// or https://, for example http://127.0.0.1:8790.")]
    NoScheme,
    #[error("That is not a web address. Use one like http://127.0.0.1:8790.")]
    NotAnAddress,
    #[error(
        "Leave the user name and password out of the address. The token goes in its own field."
    )]
    Credentials,
    #[error(
        "Over plain http://, the token would cross the internet unencrypted. Use https://, or this Mac's, a home network or a Tailscale address."
    )]
    PublicHttp,
}

impl WatcherUrl {
    /// Accepts `https://` to any host, and `http://` to loopback, a private
    /// IPv4 range (10/8, 172.16/12, 192.168/16), Tailscale (100.64/10, its
    /// IPv6 range, or a `*.ts.net` name) or `localhost`. Surrounding white
    /// space from a paste is ignored.
    pub fn parse(text: &str) -> Result<Self, WatcherUrlError> {
        let text = text.trim();
        if !(text.starts_with("http://") || text.starts_with("https://")) {
            return Err(WatcherUrlError::NoScheme);
        }
        let text = text.trim_end_matches('/');
        let url = Url::parse(text).map_err(|_| WatcherUrlError::NotAnAddress)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(WatcherUrlError::Credentials);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(WatcherUrlError::NotAnAddress);
        }
        let host = url.host().ok_or(WatcherUrlError::NotAnAddress)?;
        if url.scheme() == "http" && !private(&host) {
            return Err(WatcherUrlError::PublicHttp);
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The full URL of one of the API's routes, such as `/config`.
    pub(crate) fn route(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

impl fmt::Display for WatcherUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn private(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(ip) => private_v4(*ip),
        url::Host::Ipv6(ip) => private_v6(*ip),
        url::Host::Domain(name) => {
            let name = name.to_ascii_lowercase();
            name == "localhost" || name.ends_with(".ts.net")
        }
    }
}

fn private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        // Tailscale's carrier-grade NAT range, 100.64.0.0/10.
        || (a == 100 && (64..=127).contains(&b))
}

fn private_v6(ip: Ipv6Addr) -> bool {
    // Tailscale's IPv6 range, fd7a:115c:a1e0::/48.
    let s = ip.segments();
    ip.is_loopback() || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> String {
        WatcherUrl::parse(text)
            .unwrap_or_else(|e| panic!("{text}: {e}"))
            .to_string()
    }

    fn refused(text: &str) -> WatcherUrlError {
        WatcherUrl::parse(text).expect_err(text)
    }

    #[test]
    fn plain_http_is_accepted_where_traffic_stays_private() {
        for text in [
            "http://127.0.0.1:8790",
            "http://localhost:8790",
            "http://[::1]:8790",
            "http://10.0.0.5:8790",
            "http://172.16.0.1:8790",
            "http://172.31.255.254:8790",
            "http://192.168.1.20:8790",
            "http://100.64.0.1:8790",
            "http://100.127.255.254:8790",
            "http://[fd7a:115c:a1e0::1]:8790",
            "http://nas.tail1234.ts.net:8790",
            "http://NAS.Tail1234.TS.NET:8790",
        ] {
            ok(text);
        }
    }

    #[test]
    fn plain_http_to_a_public_host_is_refused() {
        for text in [
            "http://203.0.113.5:8790",
            "http://172.32.0.1:8790",
            "http://100.128.0.1:8790",
            "http://192.169.0.1:8790",
            "http://watcher.example.net:8790",
            "http://[2001:db8::1]:8790",
            // A name is not resolved: one that points at 127.0.0.1 is still public.
            "http://localtest.me:8790",
        ] {
            assert_eq!(refused(text), WatcherUrlError::PublicHttp, "{text}");
        }
    }

    #[test]
    fn https_is_accepted_anywhere() {
        assert_eq!(
            ok("https://watcher.example.net"),
            "https://watcher.example.net"
        );
        ok("https://203.0.113.5:8443");
    }

    #[test]
    fn an_address_without_its_scheme_is_refused() {
        assert_eq!(refused("watcher:8790"), WatcherUrlError::NoScheme);
        assert_eq!(refused("127.0.0.1:8790"), WatcherUrlError::NoScheme);
        assert_eq!(refused("ftp://127.0.0.1"), WatcherUrlError::NoScheme);
        assert_eq!(refused(""), WatcherUrlError::NoScheme);
    }

    #[test]
    fn credentials_queries_and_garbage_are_refused() {
        assert_eq!(
            refused("http://user:pw@127.0.0.1:8790"),
            WatcherUrlError::Credentials
        );
        assert_eq!(
            refused("http://token@127.0.0.1:8790"),
            WatcherUrlError::Credentials
        );
        assert_eq!(
            refused("http://127.0.0.1:8790/?a=1"),
            WatcherUrlError::NotAnAddress
        );
        assert_eq!(refused("http://"), WatcherUrlError::NotAnAddress);
        assert_eq!(
            refused("http://127.0.0.1:99999"),
            WatcherUrlError::NotAnAddress
        );
    }

    #[test]
    fn a_trailing_slash_and_white_space_are_dropped() {
        assert_eq!(ok("  http://127.0.0.1:8790/ \n"), "http://127.0.0.1:8790");
        assert_eq!(
            WatcherUrl::parse("http://127.0.0.1:8790/")
                .unwrap()
                .route("/config"),
            "http://127.0.0.1:8790/config"
        );
    }
}
