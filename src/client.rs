// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::ACCEPT;
use reqwest::{RequestBuilder, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, Error, RetStatus};

/// Melipayamak's REST endpoint.
pub const DEFAULT_BASE_URL: &str = "https://rest.payamak-panel.com";

/// Whole-request timeout used when the client builds its own HTTP client.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect timeout used when the client builds its own HTTP client.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on a response body.  The largest documented payload is a
/// page of [`Client::messages`](crate::Client::messages), whose size the
/// caller picks with `count`; this leaves room for thousands of messages.
pub const MAX_RESPONSE_BYTES: usize = 8 << 20;

const USER_AGENT: &str = concat!("melipayamak/", env!("CARGO_PKG_VERSION"));

/// A Melipayamak API client.
///
/// Cheap to clone; clones share the connection pool.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    username: String,
    api_key: String,
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

    /// Posts `fields` as `application/x-www-form-urlencoded`, after the
    /// credentials.
    pub(crate) async fn post_form(
        &self,
        path: &'static str,
        fields: Vec<(String, String)>,
    ) -> Result<Reply, Error> {
        let mut form = Vec::with_capacity(fields.len() + 2);
        form.push(("username".to_owned(), self.username.clone()));
        form.push(("password".to_owned(), self.api_key.clone()));
        form.extend(fields);
        let url = self.url(path)?;
        self.execute(self.http.post(url).form(&form)).await
    }

    /// Posts `body` as a JSON object, with the credentials added to it.
    pub(crate) async fn post_json<B>(
        &self,
        path: &'static str,
        body: &B,
    ) -> Result<Reply, Error>
    where
        B: Serialize,
    {
        let body = Credentialed {
            username: &self.username,
            password: &self.api_key,
            body,
        };
        let url = self.url(path)?;
        self.execute(self.http.post(url).json(&body)).await
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
            .extend(path.split('/'));
        Ok(url)
    }

    async fn execute(&self, req: RequestBuilder) -> Result<Reply, Error> {
        let mut resp = req
            .header(ACCEPT, "application/json")
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
        decode(status, &body)
    }
}

#[derive(Serialize)]
struct Credentialed<'a, B> {
    username: &'a str,
    password: &'a str,
    #[serde(flatten)]
    body: &'a B,
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
        if self.username.is_empty() {
            return Err(Error::InvalidArgument("username is empty"));
        }
        if self.api_key.is_empty() {
            return Err(Error::InvalidArgument("API key is empty"));
        }
        let http = match self.http {
            Some(http) => http,
            None => reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(self.timeout)
                .connect_timeout(self.connect_timeout)
                .build()
                .map_err(Error::Transport)?,
        };
        Ok(Client {
            http,
            base,
            username: self.username,
            api_key: self.api_key,
        })
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

/// The status part of a response.  `ShareService/GetBodies` nests it under
/// `MyBase`; every other call has it at the top level.
#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct Status {
    #[serde(default)]
    value: Value,
    ret_status: Option<i64>,
    str_ret_status: Option<String>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "MyBase")]
    my_base: Option<Status>,
    #[serde(flatten)]
    top: Status,
    #[serde(rename = "Data")]
    data: Option<Value>,
}

/// A response that passed the envelope checks.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    /// `Value` as text, `None` if absent or null.
    pub(crate) value: Option<String>,
    /// `Value` as received.
    pub(crate) raw_value: Value,
    pub(crate) str_ret_status: Option<String>,
    pub(crate) data: Option<Value>,
}

impl Reply {
    /// The reply as a failure: `Value` held a result code.
    pub(crate) fn api_error(&self) -> Error {
        Error::Api(ApiError {
            status: self.status,
            ret_status: Some(RetStatus::Ok),
            str_ret_status: self.str_ret_status.clone(),
            value: self.value.clone(),
        })
    }

    /// The reply as a success whose payload could not be read.
    pub(crate) fn unexpected(&self) -> Error {
        Error::UnexpectedValue {
            status: self.status,
            value: self.value.clone(),
        }
    }
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        v => Some(v.to_string()),
    }
}

/// Checks the envelope.  Passing requires a 2xx status and `RetStatus` 1;
/// anything else is an [`ApiError`].  `Value` is left to the caller, whose
/// call determines what a successful one looks like.
pub(crate) fn decode(status: u16, body: &[u8]) -> Result<Reply, Error> {
    let ok_status = (200..300).contains(&status);
    let env: Envelope = match serde_json::from_slice(body) {
        Ok(env) => env,
        Err(source) if ok_status => {
            return Err(Error::Decode { status, source });
        }
        Err(_) => {
            return Err(Error::Api(ApiError {
                status,
                ret_status: None,
                str_ret_status: None,
                value: None,
            }));
        }
    };
    let st = env.my_base.unwrap_or(env.top);
    let value = value_text(&st.value);
    if !ok_status || st.ret_status != Some(1) {
        return Err(Error::Api(ApiError {
            status,
            ret_status: st.ret_status.map(RetStatus::from_code),
            str_ret_status: st.str_ret_status,
            value,
        }));
    }
    Ok(Reply {
        status,
        value,
        raw_value: st.value,
        str_ret_status: st.str_ret_status,
        data: env.data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(r: Result<Reply, Error>) -> ApiError {
        match r {
            Err(Error::Api(e)) => e,
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn flat_success() {
        let body = br#"{"Value":"5084123456789012345","RetStatus":1,"StrRetStatus":"Ok"}"#;
        let r = decode(200, body).unwrap();
        assert_eq!(r.value.as_deref(), Some("5084123456789012345"));
        assert_eq!(r.str_ret_status.as_deref(), Some("Ok"));
        assert!(r.data.is_none());
    }

    #[test]
    fn my_base_success() {
        let body = br#"{"MyBase":{"Value":"1","RetStatus":1,"StrRetStatus":"Ok"},"Data":[]}"#;
        let r = decode(200, body).unwrap();
        assert_eq!(r.value.as_deref(), Some("1"));
        assert_eq!(r.data, Some(Value::Array(vec![])));
    }

    #[test]
    fn my_base_failure() {
        // As documented for GetBodies with a wrong API key.
        let body = br#"{"MyBase":{"Value":"0","RetStatus":0,"StrRetStatus":"UserNameAndPasswordFailed"},"Data":[]}"#;
        let e = api(decode(200, body));
        assert_eq!(e.ret_status, Some(RetStatus::UserNameAndPasswordFailed));
        assert_eq!(e.code(), Some(0));
    }

    #[test]
    fn numeric_and_null_values() {
        let r = decode(200, br#"{"Value":12.5,"RetStatus":1}"#).unwrap();
        assert_eq!(r.value.as_deref(), Some("12.5"));
        let r = decode(200, br#"{"Value":null,"RetStatus":1}"#).unwrap();
        assert_eq!(r.value, None);
        let r = decode(200, br#"{"RetStatus":1}"#).unwrap();
        assert_eq!(r.value, None);
    }

    #[test]
    fn non_json() {
        let e = api(decode(502, b"<html>bad gateway</html>"));
        assert_eq!(
            (e.status, e.ret_status, e.str_ret_status, e.value),
            (502, None, None, None)
        );
        assert!(matches!(
            decode(200, b"<html>"),
            Err(Error::Decode { status: 200, .. })
        ));
    }

    /// Every combination of status and `RetStatus`: only 2xx with
    /// `RetStatus` 1 may pass, flat or nested.
    #[test]
    fn classification_is_exhaustive() {
        let statuses = [200, 201, 299, 300, 400, 401, 404, 429, 500, 502];
        let rets = [None, Some(1), Some(0), Some(9), Some(35), Some(-1)];
        for status in statuses {
            for ret in rets {
                for nested in [false, true] {
                    let mut st = serde_json::json!({"Value": "7"});
                    if let Some(r) = ret {
                        st["RetStatus"] = r.into();
                    }
                    let env = if nested {
                        serde_json::json!({"MyBase": st, "Data": []})
                    } else {
                        st
                    };
                    let body = serde_json::to_vec(&env).unwrap();
                    let want = (200..300).contains(&status) && ret == Some(1);
                    let got = decode(status, &body);
                    assert_eq!(got.is_ok(), want, "{env} at HTTP {status}");
                    if let Err(Error::Api(e)) = got {
                        assert_eq!(e.status, status);
                        assert_eq!(e.ret_status.map(RetStatus::code), ret);
                        assert_eq!(e.value.as_deref(), Some("7"));
                    }
                }
            }
        }
    }

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
        let u = c.url("api/SendSMS/SendSMS").unwrap();
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
}
