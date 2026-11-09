// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Unofficial client for the [Melipayamak](https://www.melipayamak.com) SMS
//! REST API: sending, pattern (shared service line) sends, Smart sends,
//! delivery reports, inbox and credit, and pattern management.  Not
//! affiliated with or endorsed by Melipayamak.
//!
//! ```no_run
//! # async fn f() -> Result<(), melipayamak::Error> {
//! use melipayamak::{Client, sms::SendPattern};
//!
//! let client = Client::new(
//!     "username",
//!     std::env::var("MELIPAYAMAK_API_KEY").unwrap_or_default(),
//! )?;
//! let rec_id = client
//!     .send_pattern(&SendPattern {
//!         to: "09120000000".into(),
//!         body_id: 123456,
//!         args: vec!["4321".into()],
//!     })
//!     .await?;
//! println!("sent: {rec_id}");
//! # Ok(())
//! # }
//! ```
//!
//! Every method maps to one REST call documented at
//! <https://www.melipayamak.com/api/>; its docs name the path and the
//! documentation page.  Melipayamak's SOAP web services are not covered.
//!
//! # Semantics
//!
//! - A method returns `Ok` only if the HTTP status is 2xx, `RetStatus` is 1
//!   and `Value` has the documented successful form for that call.  Most
//!   failures come back as HTTP 200 with a numeric result code in `Value`;
//!   those are [`Error::Api`], and [`ApiError::code`] gives the code.
//! - A success whose `Value` cannot be read is [`Error::UnexpectedValue`],
//!   not [`Error::Api`]: for a send, the message may still have gone out.
//! - Credentials are the panel username and its API key, sent in the
//!   request body.  Melipayamak rejects the account password on its API
//!   (code `-110`), and only answers from IP addresses you have allowed in
//!   the panel when that setting is on.
//! - Nothing is retried.  Sends are billed and reach people; retrying is
//!   the caller's decision.  Repeated failed attempts get the calling IP
//!   blocked (code `-108`).
//! - Requests time out after [`DEFAULT_TIMEOUT`] unless configured
//!   otherwise, and response bodies are capped at [`MAX_RESPONSE_BYTES`].
//! - Inputs are not validated locally beyond what the types enforce, and
//!   beyond rejecting values that would change the meaning of a request
//!   once joined, such as a comma inside a recipient.  Melipayamak
//!   validates the rest and reports failures as [`Error::Api`].
//! - Amounts that Melipayamak sends as strings (credit, prices) are passed
//!   through as strings, so nothing is lost to floating point.
//!
//! # TLS
//!
//! The default `rustls` feature uses reqwest's rustls stack.  Disable default
//! features and enable `native-tls` to use the platform library instead
//! (OpenSSL on FreeBSD).  One of the two is required; a build without
//! either fails to compile.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// build() only accepts https outside loopback, so without TLS the client
// could never reach Melipayamak.
#[cfg(not(any(feature = "rustls", feature = "native-tls")))]
compile_error!(
    "melipayamak needs a TLS backend: enable the `rustls` (default) or \
     `native-tls` feature"
);

/// An integer-coded enum that keeps values it does not know.
macro_rules! int_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $value:literal, )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )*
            /// A value not known to this version of the crate.
            Other(i64),
        }

        impl $name {
            /// Maps a wire value.
            pub fn from_code(code: i64) -> Self {
                match code {
                    $( $value => $name::$variant, )*
                    n => $name::Other(n),
                }
            }

            /// The wire value.
            pub fn code(self) -> i64 {
                match self {
                    $( $name::$variant => $value, )*
                    $name::Other(n) => n,
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                s.serialize_i64(self.code())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(d: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                <i64 as ::serde::Deserialize>::deserialize(d)
                    .map($name::from_code)
            }
        }
    };
}

mod client;
mod error;
mod value;

pub mod pattern;
pub mod smart;
pub mod sms;

pub use client::{
    Client, ClientBuilder, DEFAULT_BASE_URL, DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_TIMEOUT, MAX_RESPONSE_BYTES,
};
pub use error::{ApiError, Error, RetStatus};
pub use reqwest;

use serde::{Deserialize, Serialize};

/// Identifies one sent message to one recipient (`recId`).  Used to ask
/// for its delivery status.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
pub struct RecId(pub i64);

impl std::fmt::Display for RecId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

int_enum! {
    /// Delivery status of one message, as reported by
    /// [`Client::delivery`] and [`Client::smart_deliveries`].
    DeliveryStatus {
        /// `-1` from [`Client::delivery`]: not sent.
        NotSent = -1,
        /// `0`: handed to the operator.
        SentToOperator = 0,
        /// `1`: delivered to the handset.
        Delivered = 1,
        /// `2`: not delivered to the handset.
        NotDelivered = 2,
        /// `3`: operator error.
        OperatorError = 3,
        /// `5`: unknown error.
        UnknownError = 5,
        /// `8`: reached the operator.
        ReachedOperator = 8,
        /// `16`: did not reach the operator.
        NotReachedOperator = 16,
        /// `35`: the recipient is on the operators' blacklist.
        Blacklisted = 35,
        /// `100`: unknown.
        Unknown = 100,
        /// `200`: sent.
        Sent = 200,
        /// `300`: filtered.
        Filtered = 300,
        /// `400`: queued for sending.
        Queued = 400,
        /// `500`: not accepted.
        Rejected = 500,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_enum_keeps_unknown_values() {
        let known: DeliveryStatus = serde_json::from_str("1").unwrap();
        assert_eq!(known, DeliveryStatus::Delivered);
        let unknown: DeliveryStatus = serde_json::from_str("7").unwrap();
        assert_eq!(unknown, DeliveryStatus::Other(7));
        for v in [known, unknown] {
            let wire = serde_json::to_string(&v).unwrap();
            assert_eq!(wire, v.code().to_string());
        }
    }

    #[test]
    fn rec_id_is_a_plain_number() {
        let id = RecId(5_084_123_456_789_012_345);
        assert_eq!(serde_json::to_string(&id).unwrap(), "5084123456789012345");
        assert_eq!(id.to_string(), "5084123456789012345");
    }
}
