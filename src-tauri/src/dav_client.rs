//! Hardened in-repo replacement for the `rustydav` 0.1.3 HTTP layer.
//!
//! `rustydav` builds its own `reqwest::blocking::Client::new()` and exposes no
//! constructor that takes a configured one, which left the security audit with
//! two documented residuals that could not be closed upstream:
//!
//! 1. **Redirect-following**: the default reqwest policy silently follows 3xx
//!    responses, so a malicious or compromised WebDAV server could bounce an
//!    authenticated request (Basic-auth header attached) to any host — an
//!    SSRF/credential-exfiltration path the URL guard cannot see, because it
//!    only validates the URL handed to the client, not the redirect target.
//! 2. **DNS rebinding TOCTOU**: the guard resolves and validates the host
//!    before the request, but reqwest re-resolves at connect time. A rebinding
//!    DNS server answers the validation lookup with a public IP and the
//!    request lookup with a private one.
//!
//! Both are closed here, at the transport, by constructing the reqwest client
//! ourselves:
//!
//! - [`reqwest::redirect::Policy::none()`]: a 3xx is returned to the caller as
//!   a plain non-success response (the app already treats non-2xx PROPFIND/GET
//!   results as fatal), so no authenticated request is ever re-sent anywhere.
//! - [`ClientBuilder::resolve_to_addrs`]: the remote host is resolved exactly
//!   once per client, every address is checked against the same blocklist the
//!   URL guard uses, and those exact addresses are pinned for the client's
//!   lifetime. Connect-time DNS is bypassed, so the address the guard checked
//!   is the address the sockets dial — rebinding has no second lookup to win.
//!
//! A client is constructed per operation (not per process), so routine DNS
//! changes are still picked up on the next operation.

use crate::webdav::{blocked_ip_reason, insecure_webdav_allowed};
use reqwest::blocking::{Body, ClientBuilder, RequestBuilder, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Url};
use std::net::ToSocketAddrs;

/// Error surfaced by [`Client`] requests.
#[derive(Debug)]
pub enum Error {
    /// The request URL could not be parsed. Deterministic: never retried.
    InvalidUrl(String),
    /// Transport-level failure, forwarded from reqwest.
    Transport(reqwest::Error),
}

impl Error {
    /// The URL the failing request targeted, when known.
    pub fn url(&self) -> Option<&Url> {
        match self {
            Error::InvalidUrl(_) => None,
            Error::Transport(e) => e.url(),
        }
    }

    /// The HTTP status of the failing response, when known.
    pub fn status(&self) -> Option<reqwest::StatusCode> {
        match self {
            Error::InvalidUrl(_) => None,
            Error::Transport(e) => e.status(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidUrl(msg) => write!(f, "invalid request URL: {msg}"),
            Error::Transport(e) => write!(f, "{e}"),
        }
    }
}

/// Minimal WebDAV HTTP client: same request semantics as `rustydav` (Basic
/// auth on every call, `application/octet-stream` PUTs, fixed-body PROPFIND),
/// built on a transport that never follows redirects and always dials the
/// addresses validated at construction time.
#[derive(Debug)]
pub struct Client {
    username: String,
    password: String,
    http: reqwest::blocking::Client,
}

impl Client {
    /// Builds a client for `base_url` using the process-wide insecure-origin
    /// policy (see [`insecure_webdav_allowed`]).
    pub fn init(username: &str, password: &str, base_url: &str) -> Result<Self, String> {
        Self::init_internal(username, password, base_url, insecure_webdav_allowed())
    }

    /// Builds a client whose transport is pinned to the addresses `base_url`
    /// resolves to at this moment.
    ///
    /// Fails closed: a host that cannot be resolved, or that resolves to any
    /// blocked address (unless `allow_insecure`, mirroring the URL guard for
    /// tests and dev overrides), is an error, not a best-effort client that
    /// quietly falls back to unpinned DNS.
    pub fn init_internal(
        username: &str,
        password: &str,
        base_url: &str,
        allow_insecure: bool,
    ) -> Result<Self, String> {
        let url =
            Url::parse(base_url).map_err(|e| format!("Invalid WebDAV URL {base_url}: {e}"))?;
        let host = url.host_str().ok_or("WebDAV URL has no host.")?;
        // `host_str()` brackets IPv6 literals; the resolver key must not have them.
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = url
            .port_or_known_default()
            .ok_or("WebDAV URL has no port.")?;

        // Resolve once, validate every address, and pin exactly these.
        let addrs: Vec<std::net::SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|e| {
                format!("Could not resolve WebDAV host {host} ({e}); refusing to connect.")
            })?
            .collect();
        if addrs.is_empty() {
            return Err(format!(
                "Could not resolve WebDAV host {host}; refusing to connect."
            ));
        }
        if !allow_insecure {
            for addr in &addrs {
                if let Some(reason) = blocked_ip_reason(addr.ip()) {
                    return Err(reason.to_string());
                }
            }
        }

        let http: reqwest::blocking::Client = ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addrs)
            .build()
            .map_err(|e| format!("Failed to build WebDAV HTTP client: {e}"))?;

        Ok(Client {
            username: username.to_owned(),
            password: password.to_owned(),
            http,
        })
    }

    fn start_request(&self, method: Method, path: &str) -> Result<RequestBuilder, Error> {
        let url = Url::parse(path).map_err(|e| Error::InvalidUrl(format!("{path}: {e}")))?;
        Ok(self
            .http
            .request(method, url)
            .basic_auth(self.username.as_str(), Some(self.password.as_str())))
    }

    fn custom_header(name: &str, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_bytes(value.as_bytes()).unwrap(),
        );
        headers
    }

    /// GET a file from the WebDAV server.
    pub fn get(&self, path: &str) -> Result<Response, Error> {
        self.start_request(Method::GET, path)?
            .send()
            .map_err(Error::Transport)
    }

    /// Upload a file to the WebDAV server.
    pub fn put<B: Into<Body>>(&self, body: B, path: &str) -> Result<Response, Error> {
        self.start_request(Method::PUT, path)?
            .headers(Self::custom_header(
                "content-type",
                "application/octet-stream",
            ))
            .body(body)
            .send()
            .map_err(Error::Transport)
    }

    /// Delete the collection or file at `path` on the WebDAV server.
    pub fn delete(&self, path: &str) -> Result<Response, Error> {
        self.start_request(Method::DELETE, path)?
            .send()
            .map_err(Error::Transport)
    }

    /// Create a directory (MKCOL) on the WebDAV server.
    pub fn mkcol(&self, path: &str) -> Result<Response, Error> {
        self.start_request(Method::from_bytes(b"MKCOL").unwrap(), path)?
            .send()
            .map_err(Error::Transport)
    }

    /// PROPFIND: list `path` (`depth` = "0", "1", or "infinity"); the response
    /// body is the multistatus XML.
    pub fn list(&self, path: &str, depth: &str) -> Result<Response, Error> {
        let body = r#"<?xml version="1.0" encoding="utf-8" ?>
            <D:propfind xmlns:D="DAV:">
                <D:allprop/>
            </D:propfind>
        "#;
        self.start_request(Method::from_bytes(b"PROPFIND").unwrap(), path)?
            .headers(Self::custom_header("depth", depth))
            .body(body)
            .send()
            .map_err(Error::Transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_mode_refuses_loopback_literal() {
        let err = Client::init_internal("u", "p", "http://127.0.0.1:8000/dav/", false)
            .expect_err("loopback literal must be refused");
        assert!(err.contains("loopback"), "unexpected error: {err}");
    }

    #[test]
    fn production_mode_refuses_loopback_hostname() {
        let err = Client::init_internal("u", "p", "http://localhost:8000/dav/", false)
            .expect_err("localhost must resolve-and-refuse");
        assert!(err.contains("loopback"), "unexpected error: {err}");
    }

    #[test]
    fn unresolvable_host_fails_closed_even_when_insecure_allowed() {
        // RFC 6761 reserves .invalid; resolution must fail or return nothing,
        // and the constructor must refuse rather than dial unpinned DNS.
        let err = Client::init_internal("u", "p", "http://rebinding-test.invalid/", true)
            .expect_err("unresolvable host must fail closed");
        assert!(
            err.contains("refusing to connect"),
            "unexpected error: {err}"
        );
    }
}
