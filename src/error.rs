// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

use std::fmt;

/// Errors returned by [`Client`](crate::Client) and by the operations in
/// [`wire`](crate::wire).
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Melipayamak answered, but not with a successful result.
    Api(ApiError),
    /// Melipayamak reported success (`RetStatus` 1), but `Value` or `Data`
    /// was not in the documented form.
    ///
    /// For a send this does not mean nothing was sent: the message may have
    /// gone out.  Check, e.g. with [`Client::messages`](crate::Client::messages),
    /// before retrying.
    UnexpectedValue {
        /// HTTP status of the response.
        status: u16,
        /// `Value` as received, if any.
        value: Option<String>,
    },
    /// The request never got a complete HTTP response: connection, TLS,
    /// timeout, or similar.
    #[cfg(feature = "client")]
    Transport(reqwest::Error),
    /// The response body was not the documented JSON envelope.
    Decode {
        /// HTTP status of the response.
        status: u16,
        /// What failed to parse.
        source: serde_json::Error,
    },
    /// The response body was larger than
    /// [`MAX_RESPONSE_BYTES`](crate::MAX_RESPONSE_BYTES).
    ResponseTooLarge,
    /// A client setting or argument was rejected before anything was sent.
    InvalidArgument(&'static str),
}

// A cause is either returned by source() or written by Display, never both,
// or reporters that walk the chain print it twice.
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api(e) => e.fmt(f),
            Error::UnexpectedValue { status, value } => {
                write!(f, "unexpected melipayamak result (HTTP {status}")?;
                if let Some(v) = value {
                    write!(f, ", Value {v:?}")?;
                }
                f.write_str(")")
            }
            #[cfg(feature = "client")]
            Error::Transport(_) => f.write_str("melipayamak request failed"),
            Error::Decode { status, .. } => {
                write!(f, "cannot decode melipayamak response (HTTP {status})")
            }
            Error::ResponseTooLarge => write!(
                f,
                "melipayamak response exceeds {} bytes",
                crate::MAX_RESPONSE_BYTES
            ),
            Error::InvalidArgument(what) => {
                write!(f, "invalid melipayamak client argument: {what}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(feature = "client")]
            Error::Transport(e) => Some(e),
            Error::Decode { source, .. } => Some(source),
            Error::Api(_)
            | Error::UnexpectedValue { .. }
            | Error::ResponseTooLarge
            | Error::InvalidArgument(_) => None,
        }
    }
}

/// An unsuccessful answer from Melipayamak.
///
/// Melipayamak reports failure in three places: the HTTP status, the
/// envelope's `RetStatus`, and `Value`, which on failure carries a numeric
/// result code whose meaning depends on the call.  All three are kept here;
/// the per-call codes are listed in the documentation page each method
/// links to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ApiError {
    /// HTTP status of the response.
    pub status: u16,
    /// The envelope's `RetStatus`, if the body had one.
    pub ret_status: Option<RetStatus>,
    /// The envelope's `StrRetStatus`, e.g. `InvalidData`.
    pub str_ret_status: Option<String>,
    /// The envelope's `Value`, as received.
    pub value: Option<String>,
}

impl ApiError {
    /// `Value` as an integer result code, e.g. `2` (insufficient credit) or
    /// `-110` (an API key is required instead of the password).
    pub fn code(&self) -> Option<i64> {
        self.value.as_deref()?.trim().parse().ok()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "melipayamak API error: HTTP {}", self.status)?;
        if let Some(r) = self.ret_status {
            write!(f, ", RetStatus {}", r.code())?;
        }
        if let Some(s) = &self.str_ret_status {
            write!(f, " ({s})")?;
        }
        if let Some(v) = &self.value {
            write!(f, ", Value {v:?}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

int_enum! {
    /// The envelope's `RetStatus`.  Names follow Melipayamak's
    /// `StrRetStatus`.
    RetStatus {
        /// `1`: the request was processed.  Not by itself proof of
        /// success; `Value` is checked too.
        Ok = 1,
        /// `0`: wrong username or API key.
        UserNameAndPasswordFailed = 0,
        /// `9`: internal error at Melipayamak.
        InternalError = 9,
        /// `25`: the operation did not succeed.
        UnSuccessful = 25,
        /// `35`: invalid data, or a security limit (IP allowlist, rate
        /// limit, API key required).  On the send calls the docs also use
        /// 35 for a recipient on the operators' blacklist.
        InvalidData = 35,
        /// `36`: the account lacks the permission.
        AccessDenied = 36,
        /// `41`: the operation is not possible in the current state.
        Impossible = 41,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    /// Walks the chain like an error reporter and checks no level repeats
    /// its cause's text.
    fn assert_no_repeats(e: &Error) {
        let mut cur: &dyn std::error::Error = e;
        while let Some(next) = cur.source() {
            let (outer, inner) = (cur.to_string(), next.to_string());
            assert!(!outer.contains(&inner), "{outer:?} repeats {inner:?}");
            cur = next;
        }
    }

    #[test]
    fn chain_has_no_repeats() {
        let source = serde_json::from_str::<u8>("x").unwrap_err();
        let decode = Error::Decode {
            status: 200,
            source,
        };
        assert!(decode.source().is_some());
        assert_no_repeats(&decode);

        let api = Error::Api(ApiError {
            status: 200,
            ret_status: Some(RetStatus::InvalidData),
            str_ret_status: Some("InvalidData".into()),
            value: Some("-110".into()),
        });
        assert!(api.source().is_none());
        assert_eq!(
            api.to_string(),
            "melipayamak API error: HTTP 200, RetStatus 35 (InvalidData), \
             Value \"-110\""
        );

        let unexpected = Error::UnexpectedValue {
            status: 200,
            value: Some("abc".into()),
        };
        assert_eq!(
            unexpected.to_string(),
            "unexpected melipayamak result (HTTP 200, Value \"abc\")"
        );
    }

    #[test]
    fn code_parses_value() {
        let mut e = ApiError {
            status: 200,
            ret_status: None,
            str_ret_status: None,
            value: Some(" -108 ".into()),
        };
        assert_eq!(e.code(), Some(-108));
        e.value = Some("NULL".into());
        assert_eq!(e.code(), None);
        e.value = None;
        assert_eq!(e.code(), None);
    }

    #[test]
    fn ret_status_round_trips() {
        for n in [i64::MIN, -1, 0, 1, 9, 25, 35, 36, 41, 42, i64::MAX] {
            assert_eq!(RetStatus::from_code(n).code(), n);
        }
        assert_eq!(RetStatus::from_code(1), RetStatus::Ok);
        assert_eq!(RetStatus::from_code(42), RetStatus::Other(42));
    }
}
