// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Sending, pattern sends, delivery reports, messages and credit
//! (`/api/SendSMS/…`).

use serde::{Deserialize, Serialize};

#[cfg(feature = "client")]
use crate::{Client, DeliveryStatus, Error, RecId, wire};

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

#[cfg(feature = "client")]
impl Client {
    /// Sends one text to one or more recipients.  Returns the `recId`s
    /// Melipayamak reports.
    ///
    /// `POST /api/SendSMS/SendSMS`
    /// (<https://www.melipayamak.com/api/sendsimplesms2/>).
    pub async fn send_sms(&self, req: &SendSms) -> Result<Vec<RecId>, Error> {
        self.call(&wire::SEND_SMS, req).await
    }

    /// Sends a one-time code in Melipayamak's fixed OTP text.
    ///
    /// `POST /api/SendSMS/SendOtp`
    /// (<https://www.melipayamak.com/api/sendotp/>).
    pub async fn send_otp(&self, req: &SendOtp) -> Result<RecId, Error> {
        self.call(&wire::SEND_OTP, req).await
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
        self.call(&wire::SEND_MULTIPLE, req).await
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
        self.call(&wire::SEND_PATTERN, req).await
    }

    /// Schedules an approved pattern send from the shared service line.
    ///
    /// `POST /api/SendSMS/BaseServiceNumberSchedule`
    /// (<https://www.melipayamak.com/api/addschedulesendbybasenumber2/>).
    pub async fn schedule_pattern(
        &self,
        req: &SchedulePattern,
    ) -> Result<RecId, Error> {
        self.call(&wire::SCHEDULE_PATTERN, req).await
    }

    /// Asks for one message's delivery status.
    ///
    /// `POST /api/SendSMS/GetDeliveries2`
    /// (<https://www.melipayamak.com/api/getdelivery/>).
    pub async fn delivery(
        &self,
        rec_id: RecId,
    ) -> Result<DeliveryStatus, Error> {
        self.call(&wire::DELIVERY, &rec_id).await
    }

    /// Lists received or sent messages.
    ///
    /// `POST /api/SendSMS/GetMessages`
    /// (<https://www.melipayamak.com/api/getmessages/>).
    pub async fn messages(
        &self,
        req: &MessagesRequest,
    ) -> Result<Vec<Message>, Error> {
        self.call(&wire::MESSAGES, req).await
    }

    /// The account's remaining credit, as Melipayamak formats it.
    ///
    /// `POST /api/SendSMS/GetCredit`
    /// (<https://www.melipayamak.com/api/getcredit/>).
    pub async fn credit(&self) -> Result<String, Error> {
        self.call(&wire::CREDIT, &()).await
    }

    /// The price of a send, in rials, as Melipayamak formats it.
    ///
    /// `POST /api/SendSMS/GetBasePrice`
    /// (<https://www.melipayamak.com/api/getprice/>).
    pub async fn base_price(
        &self,
        req: &BasePriceRequest,
    ) -> Result<String, Error> {
        self.call(&wire::BASE_PRICE, req).await
    }
}
