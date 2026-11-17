# Changelog

## 0.2.0

- `melipayamak::wire`: all 16 calls without a transport, for callers
  with their own HTTP client.  Each is a typed constant with its path,
  content type, request body and response decoding;
  `smart_deliveries` builds its operation from the IDs it asks about.
  `wire::Credentials` holds the username and API key that every body
  carries, and never prints them.  `Client` sends through the same code,
  and its bodies are byte for byte those of 0.1.
- The client is now the `client` feature, still on by default through
  `rustls` or `native-tls`.  With no features the crate depends only on
  serde and serde_json.

## 0.1.0

- First release: all 16 REST calls in Melipayamak's documentation of
  9 November 2026.
