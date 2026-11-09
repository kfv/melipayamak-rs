// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Sending, pattern sends, delivery reports, messages and credit
//! (`/api/SendSMS/…`).

use serde::{Deserialize, Serialize};

use crate::{Client, DeliveryStatus, Error, RecId, value};

/// Request for [`Client::send_sms`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendSms {
    /// Recipients' mobile numbers.  Sending to more than one requires the
    /// opt-out phrase («لغو11») in the text.
    pub to: Vec<String>,
    /// Sender: one of the account's dedicated numbers.
    pub from: String,
    /// Message text.
    pub text: String,
    /// Send as a flash message.
    pub flash: bool,
}

/// Request for [`Client::send_otp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOtp {
    /// Recipient's mobile number.
    pub to: String,
    /// Sender: one of the account's dedicated numbers.
    pub from: String,
    /// The code, sent in Melipayamak's fixed OTP text.  Melipayamak takes
    /// it as an integer, so leading zeros are not kept.
    pub code: u32,
}

/// One recipient and its text, for the one-to-one sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// Recipient's mobile number.
    pub to: String,
    /// Text for this recipient.
    pub text: String,
}

/// Request for [`Client::send_multiple`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendMultiple {
    /// Sender: one of the account's dedicated numbers.
    pub from: String,
    /// The messages, each with its own recipient and text.
    pub messages: Vec<Outgoing>,
}

/// Request for [`Client::send_pattern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPattern {
    /// Recipient's mobile number; exactly one.
    pub to: String,
    /// The approved pattern (`bodyId`).
    pub body_id: i32,
    /// Values for the pattern's variables, in order.
    pub args: Vec<String>,
}

/// Request for [`Client::schedule_pattern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulePattern {
    /// Recipient's mobile number; exactly one.
    pub to: String,
    /// The approved pattern (`bodyId`).
    pub body_id: i32,
    /// Values for the pattern's variables, in order.
    pub args: Vec<String>,
    /// When to send, as `YYYY-MM-DDTHH:MM:SS`, e.g. `2024-08-04T15:20:00`.
    /// Melipayamak only accepts times between 07:00 and 22:00.
    pub schedule_date: String,
}

int_enum! {
    /// Which messages [`Client::messages`] lists.
    Location {
        /// `1`: received.
        Received = 1,
        /// `2`: sent.
        Sent = 2,
        /// `-1`: all.
        All = -1,
    }
}

/// Request for [`Client::messages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesRequest {
    /// Received, sent, or both.
    pub location: Location,
    /// Only messages on this number of the account; all numbers if `None`.
    pub from: Option<String>,
    /// Index of the first message to return; Melipayamak suggests 0.
    pub index: u32,
    /// How many messages to return.
    pub count: u32,
}

/// A sent or received message, as listed by [`Client::messages`].  Field
/// names follow Melipayamak's `MessagesBL` record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub struct Message {
    /// Message ID.
    #[serde(rename = "MsgID")]
    pub msg_id: i64,
    /// Text.
    pub body: Option<String>,
    /// When it was sent or received, as Melipayamak formats it.
    pub send_date: Option<String>,
    /// Sending number.
    pub sender: Option<String>,
    /// Receiving number.
    pub receiver: Option<String>,
    /// Folder it was first stored in.
    pub first_location: Option<i64>,
    /// Folder it is in now.
    pub current_location: Option<i64>,
    /// Number of SMS parts.
    pub parts: Option<i64>,
    /// Whether it was a flash message.
    pub is_flash: Option<bool>,
    /// Whether it has been read.
    pub is_read: Option<bool>,
    /// Whether it was sent as Unicode.
    pub is_unicode: Option<bool>,
    /// Credit charged.  Melipayamak itself holds this as a binary
    /// floating-point number, so `f64` loses nothing.
    pub credit: Option<f64>,
    /// Number of recipients.
    pub rec_count: Option<i64>,
    /// Recipients that failed.
    pub rec_failed: Option<i64>,
    /// Recipients that succeeded.
    pub rec_success: Option<i64>,
}

/// Request for [`Client::base_price`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasePriceRequest {
    /// Number of Irancell recipients.
    pub irancell_count: u32,
    /// Number of MCI (Hamrah-e Aval) recipients.
    pub mtn_count: u32,
    /// Sender.
    pub from: String,
    /// Message text.
    pub text: String,
}

/// Joins `items` with `sep`, refusing any item that contains `sep`, since
/// that would silently change the list Melipayamak sees.
pub(crate) fn join(
    items: &[String],
    sep: char,
    what: &'static str,
) -> Result<String, Error> {
    if items.iter().any(|s| s.contains(sep)) {
        return Err(Error::InvalidArgument(what));
    }
    Ok(items.join(&sep.to_string()))
}

pub(crate) fn recipients(to: &[String]) -> Result<String, Error> {
    if to.is_empty() {
        return Err(Error::InvalidArgument("no recipients"));
    }
    join(to, ',', "recipient contains a comma")
}

fn pattern_args(args: &[String]) -> Result<String, Error> {
    join(args, ';', "pattern argument contains a semicolon")
}

/// `name[0]=…&name[1]=…`, the form array encoding the documentation's
/// samples use.
pub(crate) fn indexed<'a>(
    name: &str,
    items: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| (format!("{name}[{i}]"), v.to_owned()))
        .collect()
}

fn pair(k: &str, v: impl ToString) -> (String, String) {
    (k.to_owned(), v.to_string())
}

#[derive(Serialize)]
struct SendMultipleBody<'a> {
    to: Vec<&'a str>,
    from: &'a str,
    text: Vec<&'a str>,
}

impl Client {
    /// Sends one text to one or more recipients.  Returns the `recId`s
    /// Melipayamak reports.
    ///
    /// `POST /api/SendSMS/SendSMS`
    /// (<https://www.melipayamak.com/api/sendsimplesms2/>).
    pub async fn send_sms(&self, req: &SendSms) -> Result<Vec<RecId>, Error> {
        let fields = vec![
            pair("to", recipients(&req.to)?),
            pair("from", &req.from),
            pair("text", &req.text),
            pair("isflash", req.flash),
        ];
        let r = self.post_form("api/SendSMS/SendSMS", fields).await?;
        value::rec_ids(&r)
    }

    /// Sends a one-time code in Melipayamak's fixed OTP text.
    ///
    /// `POST /api/SendSMS/SendOtp`
    /// (<https://www.melipayamak.com/api/sendotp/>).
    pub async fn send_otp(&self, req: &SendOtp) -> Result<RecId, Error> {
        let fields = vec![
            pair("to", &req.to),
            pair("from", &req.from),
            pair("code", req.code),
        ];
        let r = self.post_form("api/SendSMS/SendOtp", fields).await?;
        value::rec_id(&r)
    }

    /// Sends a different text to each recipient.  Returns the `recId`s
    /// Melipayamak reports, which may be none: its docs also list a bare
    /// `1` as success for this call.
    ///
    /// `POST /api/SendSMS/SendMultipleSMS`, JSON body
    /// (<https://www.melipayamak.com/api/sendmultiplesms/>).
    pub async fn send_multiple(
        &self,
        req: &SendMultiple,
    ) -> Result<Vec<RecId>, Error> {
        if req.messages.is_empty() {
            return Err(Error::InvalidArgument("no recipients"));
        }
        let body = SendMultipleBody {
            to: req.messages.iter().map(|m| m.to.as_str()).collect(),
            from: &req.from,
            text: req.messages.iter().map(|m| m.text.as_str()).collect(),
        };
        let r = self.post_json("api/SendSMS/SendMultipleSMS", &body).await?;
        if r.value.as_deref().map(str::trim) == Some("1") {
            return Ok(Vec::new());
        }
        value::rec_ids(&r)
    }

    /// Sends an approved pattern from the shared service line, which also
    /// reaches numbers on the operators' blacklist.
    ///
    /// `POST /api/SendSMS/BaseServiceNumber`
    /// (<https://www.melipayamak.com/api/sendbybasenumber2/>).
    pub async fn send_pattern(
        &self,
        req: &SendPattern,
    ) -> Result<RecId, Error> {
        let fields = vec![
            pair("text", pattern_args(&req.args)?),
            pair("to", &req.to),
            pair("bodyId", req.body_id),
        ];
        let r = self
            .post_form("api/SendSMS/BaseServiceNumber", fields)
            .await?;
        value::rec_id(&r)
    }

    /// Schedules an approved pattern send from the shared service line.
    ///
    /// `POST /api/SendSMS/BaseServiceNumberSchedule`
    /// (<https://www.melipayamak.com/api/addschedulesendbybasenumber2/>).
    pub async fn schedule_pattern(
        &self,
        req: &SchedulePattern,
    ) -> Result<RecId, Error> {
        let fields = vec![
            pair("text", pattern_args(&req.args)?),
            pair("to", &req.to),
            pair("bodyId", req.body_id),
            pair("scheduleDate", &req.schedule_date),
        ];
        let r = self
            .post_form("api/SendSMS/BaseServiceNumberSchedule", fields)
            .await?;
        value::rec_id(&r)
    }

    /// Asks for one message's delivery status.
    ///
    /// `POST /api/SendSMS/GetDeliveries2`
    /// (<https://www.melipayamak.com/api/getdelivery/>).
    pub async fn delivery(
        &self,
        rec_id: RecId,
    ) -> Result<DeliveryStatus, Error> {
        let fields = vec![pair("recId", rec_id)];
        let r = self.post_form("api/SendSMS/GetDeliveries2", fields).await?;
        value::delivery(&r)
    }

    /// Lists received or sent messages.
    ///
    /// `POST /api/SendSMS/GetMessages`
    /// (<https://www.melipayamak.com/api/getmessages/>).
    pub async fn messages(
        &self,
        req: &MessagesRequest,
    ) -> Result<Vec<Message>, Error> {
        let fields = vec![
            pair("location", req.location.code()),
            pair("from", req.from.as_deref().unwrap_or_default()),
            pair("index", req.index),
            pair("count", req.count),
        ];
        let r = self.post_form("api/SendSMS/GetMessages", fields).await?;
        value::records(&r)
    }

    /// The account's remaining credit, as Melipayamak formats it.
    ///
    /// `POST /api/SendSMS/GetCredit`
    /// (<https://www.melipayamak.com/api/getcredit/>).
    pub async fn credit(&self) -> Result<String, Error> {
        let r = self.post_form("api/SendSMS/GetCredit", Vec::new()).await?;
        value::amount(&r)
    }

    /// The price of a send, in rials, as Melipayamak formats it.
    ///
    /// `POST /api/SendSMS/GetBasePrice`
    /// (<https://www.melipayamak.com/api/getprice/>).
    pub async fn base_price(
        &self,
        req: &BasePriceRequest,
    ) -> Result<String, Error> {
        let fields = vec![
            pair("irancellCount", req.irancell_count),
            pair("mtnCount", req.mtn_count),
            pair("from", &req.from),
            pair("text", &req.text),
        ];
        let r = self.post_form("api/SendSMS/GetBasePrice", fields).await?;
        value::amount(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_refuse_their_separator() {
        let v =
            |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(recipients(&v(&["0912", "0935"])).unwrap(), "0912,0935");
        assert!(recipients(&[]).is_err());
        assert!(recipients(&v(&["0912,0935"])).is_err());
        assert_eq!(pattern_args(&v(&["a", "b c"])).unwrap(), "a;b c");
        assert_eq!(pattern_args(&[]).unwrap(), "");
        assert!(pattern_args(&v(&["a;b"])).is_err());
    }

    #[test]
    fn indexed_names() {
        assert_eq!(
            indexed("Ids", ["7", "8"]),
            vec![("Ids[0]".into(), "7".into()), ("Ids[1]".into(), "8".into())]
        );
    }
}
