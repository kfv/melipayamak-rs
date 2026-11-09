// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Reading `Value` once the envelope has passed.
//!
//! Melipayamak puts both the result and the failure code in `Value`, and
//! `RetStatus` 1 does not rule out a failure code there.  These helpers
//! decide which one a `Value` is, per the shape each call documents, and
//! never read an unrecognised `Value` as a success.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::Reply;
use crate::{DeliveryStatus, Error, RecId};

/// Every result code documented for the REST calls lies strictly inside
/// ±`CODE_BOUND` (the largest is 500, "not accepted", a delivery status).
/// A `recId` is far larger; the pattern-send docs describe a successful one
/// as having more than 15 digits.
const CODE_BOUND: i64 = 1000;

fn as_int(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

fn is_code(n: i64) -> bool {
    n.abs() < CODE_BOUND
}

/// One or more comma-separated `recId`s.  A lone result code is a failure.
pub(crate) fn rec_ids(r: &Reply) -> Result<Vec<RecId>, Error> {
    let Some(v) = r.value.as_deref() else {
        return Err(r.unexpected());
    };
    let parts: Vec<Option<i64>> = v.split(',').map(as_int).collect();
    if let [Some(n)] = parts[..]
        && is_code(n)
    {
        return Err(r.api_error());
    }
    parts
        .into_iter()
        .map(|p| match p {
            Some(n) if n >= CODE_BOUND => Ok(RecId(n)),
            _ => Err(r.unexpected()),
        })
        .collect()
}

/// Exactly one `recId`.
pub(crate) fn rec_id(r: &Reply) -> Result<RecId, Error> {
    match rec_ids(r)?[..] {
        [id] => Ok(id),
        _ => Err(r.unexpected()),
    }
}

/// `Value` "1", the documented success of calls that return nothing else.
pub(crate) fn one(r: &Reply) -> Result<(), Error> {
    match r.value.as_deref().and_then(as_int) {
        Some(1) => Ok(()),
        Some(_) => Err(r.api_error()),
        None => Err(r.unexpected()),
    }
}

/// A positive integer ID; zero and negatives are result codes.
pub(crate) fn positive_id(r: &Reply) -> Result<i64, Error> {
    match r.value.as_deref().and_then(as_int) {
        Some(n) if n > 0 => Ok(n),
        Some(_) => Err(r.api_error()),
        None => Err(r.unexpected()),
    }
}

/// A non-negative decimal amount, passed through as text.  Negative codes,
/// and the `NULL` that `GetCredit` documents for a failure, are failures.
pub(crate) fn amount(r: &Reply) -> Result<String, Error> {
    let Some(v) = r.value.as_deref().map(str::trim) else {
        return Err(r.api_error());
    };
    if v.eq_ignore_ascii_case("null") || v.starts_with('-') {
        return Err(r.api_error());
    }
    let (int, frac) = v.split_once('.').unwrap_or((v, "0"));
    let digits =
        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if digits(int) && digits(frac) {
        Ok(v.to_owned())
    } else {
        Err(r.unexpected())
    }
}

/// The delivery status of one message (`SendSMS/GetDeliveries2`).  `null`
/// means delivery reports are disabled for the account; other negatives
/// except `-1` (not sent) are failures of the call.
pub(crate) fn delivery(r: &Reply) -> Result<DeliveryStatus, Error> {
    match r.value.as_deref().and_then(as_int) {
        None if r.value.is_none() => Err(r.api_error()),
        None => Err(r.unexpected()),
        Some(n) if n < -1 => Err(r.api_error()),
        Some(n) => Ok(DeliveryStatus::from_code(n)),
    }
}

/// Integers from `Data` if it is an array, else from a comma-separated
/// `Value`, else from `Value` if it is an array.  The docs do not show
/// which of these the list calls use.
pub(crate) fn int_list(r: &Reply) -> Result<Vec<i64>, Error> {
    let from_array = |a: &[Value]| -> Option<Vec<i64>> {
        a.iter()
            .map(|v| match v {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => as_int(s),
                _ => None,
            })
            .collect()
    };
    let list = match (&r.data, &r.raw_value) {
        (Some(Value::Array(a)), _) | (_, Value::Array(a)) => from_array(a),
        _ => r
            .value
            .as_deref()
            .and_then(|v| v.split(',').map(as_int).collect()),
    };
    list.ok_or_else(|| r.unexpected())
}

/// Records from `Data`, or from `Value` if that is the array.  Without
/// either, a zero or negative `Value` is a result code; anything else is
/// not read as an empty list.
pub(crate) fn records<T: DeserializeOwned>(r: &Reply) -> Result<Vec<T>, Error> {
    let list = match (&r.data, &r.raw_value) {
        (Some(d @ Value::Array(_)), _) | (_, d @ Value::Array(_)) => d,
        _ => {
            return Err(match r.value.as_deref().and_then(as_int) {
                Some(n) if n <= 0 => r.api_error(),
                _ => r.unexpected(),
            });
        }
    };
    serde_json::from_value(list.clone()).map_err(|source| Error::Decode {
        status: r.status,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::decode;

    fn reply(value: &str) -> Reply {
        let body =
            format!(r#"{{"Value":{value},"RetStatus":1,"StrRetStatus":"Ok"}}"#);
        decode(200, body.as_bytes()).unwrap()
    }

    fn is_api<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
        matches!(r, Err(Error::Api(_)))
    }

    fn is_unexpected<T: std::fmt::Debug>(r: Result<T, Error>) -> bool {
        matches!(r, Err(Error::UnexpectedValue { .. }))
    }

    #[test]
    fn rec_ids_split_codes_from_ids() {
        let id = "5084123456789012345";
        assert_eq!(
            rec_id(&reply(&format!("\"{id}\""))).unwrap().to_string(),
            id
        );
        assert_eq!(
            rec_ids(&reply(&format!("\"{id},{id}\""))).unwrap().len(),
            2
        );
        // Documented failure codes, positive and negative.
        for code in ["0", "2", "11", "35", "-5", "-110", "-111"] {
            assert!(is_api(rec_ids(&reply(&format!("\"{code}\"")))), "{code}");
        }
        // Nothing that might hide a send is read as success, nor as a
        // plain failure.
        for odd in [
            "\"\"",
            "\"abc\"",
            "\"12x\"",
            "null",
            "\"1,2\"",
            "\"5084123456789012345,7\"",
        ] {
            assert!(is_unexpected(rec_ids(&reply(odd))), "{odd}");
        }
        assert!(is_unexpected(rec_id(&reply(&format!("\"{id},{id}\"")))));
    }

    #[test]
    fn one_and_positive_id() {
        assert!(one(&reply("\"1\"")).is_ok());
        assert!(one(&reply("1")).is_ok());
        assert!(is_api(one(&reply("\"-4\""))));
        assert!(is_api(one(&reply("\"0\""))));
        assert!(is_unexpected(one(&reply("\"ok\""))));
        assert_eq!(positive_id(&reply("\"4711\"")).unwrap(), 4711);
        assert!(is_api(positive_id(&reply("\"0\""))));
        assert!(is_api(positive_id(&reply("\"-14\""))));
        assert!(is_unexpected(positive_id(&reply("null"))));
    }

    #[test]
    fn amounts() {
        assert_eq!(amount(&reply("\"1234\"")).unwrap(), "1234");
        assert_eq!(amount(&reply("\"1234.565\"")).unwrap(), "1234.565");
        assert_eq!(amount(&reply("\"0\"")).unwrap(), "0");
        for bad in ["\"NULL\"", "null", "\"-1\"", "\"-2\"", "\"-110\""] {
            assert!(is_api(amount(&reply(bad))), "{bad}");
        }
        for odd in ["\"\"", "\"1.\"", "\".5\"", "\"1e3\"", "\"x\""] {
            assert!(is_unexpected(amount(&reply(odd))), "{odd}");
        }
    }

    #[test]
    fn delivery_statuses() {
        assert_eq!(
            delivery(&reply("\"1\"")).unwrap(),
            DeliveryStatus::Delivered
        );
        assert_eq!(
            delivery(&reply("\"-1\"")).unwrap(),
            DeliveryStatus::NotSent
        );
        assert_eq!(
            delivery(&reply("\"500\"")).unwrap(),
            DeliveryStatus::Rejected
        );
        for bad in ["null", "\"-2\"", "\"-3\"", "\"-10\"", "\"-108\""] {
            assert!(is_api(delivery(&reply(bad))), "{bad}");
        }
        assert!(is_unexpected(delivery(&reply("\"x\""))));
    }

    #[test]
    fn int_lists() {
        assert_eq!(int_list(&reply("\"1,-2,8\"")).unwrap(), vec![1, -2, 8]);
        assert_eq!(int_list(&reply("[1,\"2\"]")).unwrap(), vec![1, 2]);
        let r = decode(200, br#"{"Value":"1","RetStatus":1,"Data":[0,1]}"#)
            .unwrap();
        assert_eq!(int_list(&r).unwrap(), vec![0, 1]);
        assert!(is_unexpected(int_list(&reply("\"1,x\""))));
        assert!(is_unexpected(int_list(&reply("null"))));
    }

    #[test]
    fn record_lists() {
        let r = decode(200, br#"{"Value":"1","RetStatus":1,"Data":[3,4]}"#)
            .unwrap();
        assert_eq!(records::<u8>(&r).unwrap(), vec![3, 4]);
        assert_eq!(records::<u8>(&reply("[5]")).unwrap(), vec![5]);
        assert!(is_unexpected(records::<u8>(&reply("null"))));
        assert!(is_unexpected(records::<u8>(&reply("\"1\""))));
        assert!(is_api(records::<u8>(&reply("\"-110\""))));
        assert!(is_api(records::<u8>(&reply("\"0\""))));
        let r = decode(200, br#"{"RetStatus":1,"Data":["x"]}"#).unwrap();
        assert!(matches!(records::<u8>(&r), Err(Error::Decode { .. })));
    }
}
