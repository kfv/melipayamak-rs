# melipayamak

Unofficial Rust client for the [Melipayamak](https://www.melipayamak.com) SMS
REST API: plain, OTP and one-to-one sends, pattern sends from the shared
service line (immediate and scheduled), Smart sends with backup senders,
delivery reports, message lists, credit and prices, and pattern management.

Not affiliated with or endorsed by Melipayamak.

```toml
[dependencies]
melipayamak = "0.2"
```

```rust
use melipayamak::{Client, RecId, sms::SendPattern};

async fn send_code(user: &str, api_key: &str) -> Result<RecId, melipayamak::Error> {
    let client = Client::new(user, api_key)?;
    client
        .send_pattern(&SendPattern {
            to: "09120000000".into(),
            body_id: 123456,
            args: vec!["4321".into()],
        })
        .await
}
```

## Behaviour

- A call succeeds only if the HTTP status is 2xx, `RetStatus` is 1 and
  `Value` has the documented successful form for that call.  Melipayamak
  returns most failures as HTTP 200 with a result code in `Value`; those
  are errors here, and `ApiError::code` gives the code.
- A reported success whose `Value` cannot be read is
  `Error::UnexpectedValue`, kept apart from plain failures: for a send, the
  message may have gone out.
- Credentials are the panel username and API key, sent in the request
  body.  Melipayamak refuses the account password on its API.
- Nothing is retried: sends are billed and reach people, and repeated
  failures get the calling IP blocked.
- Requests time out after 30 seconds by default; responses are capped at
  8 MiB.

## Features and TLS

The client is the `client` feature, on by default through `rustls`, which
uses reqwest's rustls stack.  For the platform library instead (OpenSSL on
FreeBSD):

```toml
melipayamak = { version = "0.2", default-features = false, features = ["native-tls"] }
```

The client needs a TLS backend; a build with `client` and neither fails to
compile.

## Your own HTTP client

With no features at all the crate is the request and response types plus
`melipayamak::wire`: every call's path, content type, request body and
response decoding, under the same success rule.  Nothing then depends on
reqwest or tokio, and requests go through whatever HTTP client you already
have.

```toml
melipayamak = { version = "0.2", default-features = false }
```

```rust
use melipayamak::{wire, sms::SendPattern};

let creds = wire::Credentials::new(user, api_key)?;
let req = SendPattern {
    to: "09120000000".into(),
    body_id: 123456,
    args: vec!["4321".into()],
};
let op = wire::SEND_PATTERN;
let path = op.path(); // POST base URL + path
let content_type = op.content_type(); // form-encoded here; JSON for some
let body = op.body(&creds, &req)?; // carries the API key
// ... send it, read at most wire::MAX_RESPONSE_BYTES, then:
let rec_id = op.decode(status, &response_body)?;
```

Melipayamak takes the credentials in the body, so every body carries the
API key: keep bodies out of logs.  `Credentials` never prints the key.
`wire::smart_deliveries` builds its operation from the IDs it asks about,
which its decoding checks the answer against.  The client sends through
the same code, so the two can't drift.

## Supported Rust versions

The latest stable release and the two before it ("N-2").  `rust-version`
is currently 1.97, and is raised as new releases come out.

## API coverage

All 16 REST calls in Melipayamak's documentation at
<https://www.melipayamak.com/api/> as of 9 November 2026: nine under
`SendSMS`, three under `SmartSMS` and four under `ShareService`.  The SOAP
web services and the token-based console API are not covered.

The documentation shows the request of every call, but not where some
responses carry their payload.  For `GetMessages`, `SendMultipleSMS`,
`SmartSMS/SendMultiple` and `SmartSMS/GetDeliveries` the client accepts the
list in `Data` or in `Value`, and treats anything else as
`Error::UnexpectedValue`.  These have not been checked against a live
account.

## Licence

Licensed under BSD-2-Clause (see `LICENSE`).
