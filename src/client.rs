// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;
use reqwest::header::{ACCEPT, CONTENT_TYPE};

use crate::error::Error;
use crate::wire::{
    Credentials, DEFAULT_BASE_URL, MAX_RESPONSE_BYTES, Operation,
};

/// Whole-request timeout used when the client builds its own HTTP client.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect timeout used when the client builds its own HTTP client.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const USER_AGENT: &str = concat!("melipayamak/", env!("CARGO_PKG_VERSION"));

/// A Melipayamak API client.
///
/// Cheap to clone; clones share the connection pool.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    creds: Credentials,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a client for the production endpoint with default settings.
    /// `api_key` is the key from the panel's settings, not the account
    /// password.
    pub fn new(
        username: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, Error> {
        Self::builder(username, api_key).build()
    }

    /// Starts configuring a client.
    pub fn builder(
        username: impl Into<String>,
        api_key: impl Into<String>,
    ) -> ClientBuilder {
        ClientBuilder {
            username: username.into(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            http: None,
        }
    }

    /// The base URL requests are sent to.
    pub fn base_url(&self) -> &str {
        self.base.as_str()
    }

    /// The credentials, for the operations in [`crate::wire`] that are not
    /// an [`Operation`].
    pub(crate) fn credentials(&self) -> &Credentials {
        &self.creds
    }

    /// Sends one operation; see [`crate::wire`].  A request the operation
    /// refuses is never sent.
    pub(crate) async fn call<Req, Resp>(
        &self,
        op: &Operation<Req, Resp>,
        req: &Req,
    ) -> Result<Resp, Error> {
        let body = op.body(&self.creds, req)?;
        let (status, bytes) =
            self.post(op.path(), op.content_type(), body).await?;
        op.decode(status, &bytes)
    }

    /// Posts `body` to `path`; the status and at most
    /// [`MAX_RESPONSE_BYTES`] of body.
    pub(crate) async fn post(
        &self,
        path: &'static str,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), Error> {
        let mut resp = self
            .http
            .post(self.url(path)?)
            .header(CONTENT_TYPE, content_type)
            .header(ACCEPT, "application/json")
            .body(body)
            .send()
            .await
            .map_err(Error::Transport)?;
        let status = resp.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(Error::Transport)? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(Error::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }

    /// Appends the path's segments to the base URL.
    fn url(&self, path: &'static str) -> Result<Url, Error> {
        let mut url = self.base.clone();
        // Cannot fail: build() rejects cannot-be-a-base URLs.
        url.path_segments_mut()
            .map_err(|()| {
                Error::InvalidArgument("base URL cannot have a path")
            })?
            .pop_if_empty()
            .extend(path.strip_prefix('/').unwrap_or(path).split('/'));
        Ok(url)
    }
}

/// Configures a [`Client`].
pub struct ClientBuilder {
    username: String,
    api_key: String,
    base_url: String,
    timeout: Duration,
    connect_timeout: Duration,
    http: Option<reqwest::Client>,
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("http", &self.http)
            .finish_non_exhaustive()
    }
}

impl ClientBuilder {
    /// Sends requests somewhere other than [`DEFAULT_BASE_URL`].  Must be
    /// `https`, except for loopback hosts.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Whole-request timeout.  Ignored if [`http_client`](Self::http_client)
    /// is set.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Connect timeout.  Ignored if [`http_client`](Self::http_client) is
    /// set.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Uses a preconfigured HTTP client, e.g. to set a proxy or share a
    /// connection pool.  Its timeouts apply instead of this builder's.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Validates the settings and creates the client.
    pub fn build(self) -> Result<Client, Error> {
        let base = Url::parse(&self.base_url)
            .map_err(|_| Error::InvalidArgument("base URL does not parse"))?;
        if base.cannot_be_a_base()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::InvalidArgument(
                "base URL must be a plain scheme://host[/path]",
            ));
        }
        // The credentials travel in the body of every request.
        match base.scheme() {
            "https" => {}
            "http" if is_loopback(&base) => {}
            _ => return Err(Error::InvalidArgument("base URL must use https")),
        }
        let creds = Credentials::new(self.username, self.api_key)?;
        let http = match self.http {
            Some(http) => http,
            None => reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(self.timeout)
                .connect_timeout(self.connect_timeout)
                .build()
                .map_err(Error::Transport)?,
        };
        Ok(Client { http, base, creds })
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(h) => h
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_checks() {
        let bad = |url: &str| {
            matches!(
                Client::builder("u", "k").base_url(url).build(),
                Err(Error::InvalidArgument(_))
            )
        };
        assert!(bad("http://rest.payamak-panel.com"));
        assert!(bad("ftp://rest.payamak-panel.com"));
        assert!(bad("https://rest.payamak-panel.com/?x=1"));
        assert!(bad("mailto:x@y"));
        assert!(bad("not a url"));
        for ok in [
            "http://127.0.0.1:8080",
            "http://[::1]:1",
            "http://localhost",
        ] {
            assert!(
                Client::builder("u", "k").base_url(ok).build().is_ok(),
                "{ok}"
            );
        }
        assert!(matches!(
            Client::new("", "k"),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            Client::new("u", ""),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn paths() {
        let c = Client::builder("u", "k")
            .base_url("https://example.test/proxy/")
            .build()
            .unwrap();
        let u = c.url("/api/SendSMS/SendSMS").unwrap();
        assert_eq!(
            u.as_str(),
            "https://example.test/proxy/api/SendSMS/SendSMS"
        );
    }

    #[test]
    fn credentials_are_not_printed() {
        let (user, key) = ("someuser", "s3cr3t-k3y");
        let b = Client::builder(user, key);
        let shown = format!("{b:?}");
        assert!(!shown.contains(key) && !shown.contains(user), "{shown}");
        let c = b.build().unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains(key) && !shown.contains(user), "{shown}");
    }

    /// The body reqwest's `.form()` makes of `pairs`.
    fn reqwest_form(pairs: &[(&str, &str)]) -> String {
        let req = reqwest::Client::new()
            .post("http://example.test/")
            .form(pairs)
            .build()
            .unwrap();
        let body = req.body().and_then(|b| b.as_bytes()).unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    /// The wire layer form-encodes as reqwest's `.form()` does, which 0.1
    /// sent through, so the bodies on the wire have not changed.
    #[test]
    fn wire_form_matches_reqwest() {
        let awkward = [
            "",
            "plain",
            "a b",
            "a+b",
            "a&b=c",
            "50%",
            "%2B",
            "سلام لغو11",
            "\u{0}\u{1}\t\n\r\u{1f}\u{7f}",
            "*-._~!$'(),;:@/?#[]{}|\\^`\"<>",
            "\u{a0}\u{200c}\u{feff}😀",
        ];
        let (user, key) = ("user name+1", "k&e=y%");
        let creds = Credentials::new(user, key).unwrap();
        for v in awkward {
            let fields = [(v, v), ("to[0]", v)];
            let mut pairs = vec![("username", user), ("password", key)];
            pairs.extend(fields);
            assert_eq!(
                crate::wire::encode_form(&creds, &fields),
                reqwest_form(&pairs),
                "{v:?}"
            );
        }

        // A whole operation, as 0.1 built it.
        let req = crate::sms::SendPattern {
            to: "0912 & 0935".into(),
            body_id: 7,
            args: awkward.iter().map(|s| s.replace(';', "")).collect(),
        };
        let body = crate::wire::SEND_PATTERN.body(&creds, &req).unwrap();
        let text = req.args.join(";");
        let want = reqwest_form(&[
            ("username", user),
            ("password", key),
            ("text", &text),
            ("to", "0912 & 0935"),
            ("bodyId", "7"),
        ]);
        assert_eq!(String::from_utf8(body).unwrap(), want);
    }
}
