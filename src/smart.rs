// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Smart sends, which retry from up to two backup numbers
//! (`/api/SmartSMS/…`).

use crate::sms::Outgoing;
#[cfg(feature = "client")]
use crate::{Client, DeliveryStatus, Error, RecId, wire};

/// Senders for a Smart send.  Melipayamak tries `from` first, then
/// `from_support_one`, then `from_support_two`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartSenders {
    /// First choice.
    pub from: String,
    /// Used if the first attempt fails.
    pub from_support_one: String,
    /// Used if the second attempt fails.
    pub from_support_two: String,
}

/// Request for [`Client::smart_send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartSend {
    /// Recipients' mobile numbers.
    pub to: Vec<String>,
    /// Message text.
    pub text: String,
    /// Senders, in the order they are tried.
    pub senders: SmartSenders,
}

/// Request for [`Client::smart_send_multiple`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartSendMultiple {
    /// The messages, each with its own recipient and text; at most 100.
    pub messages: Vec<Outgoing>,
    /// Senders, in the order they are tried.
    pub senders: SmartSenders,
}

#[cfg(feature = "client")]
impl Client {
    /// Sends one text to one or more recipients, falling back to the
    /// backup senders.  Returns one ID per recipient.
    ///
    /// `POST /api/SmartSMS/Send`
    /// (<https://www.melipayamak.com/api/sendsmartsms/>).
    pub async fn smart_send(
        &self,
        req: &SmartSend,
    ) -> Result<Vec<RecId>, Error> {
        self.call(&wire::SMART_SEND, req).await
    }

    /// Sends a different text to each recipient, falling back to the
    /// backup senders.  Returns one ID per recipient.
    ///
    /// `POST /api/SmartSMS/SendMultiple`
    /// (<https://www.melipayamak.com/api/sendmultiplesmartsms/>).
    pub async fn smart_send_multiple(
        &self,
        req: &SmartSendMultiple,
    ) -> Result<Vec<RecId>, Error> {
        self.call(&wire::SMART_SEND_MULTIPLE, req).await
    }

    /// Asks for the delivery status of Smart sends, one per ID, in order.
    /// Besides the usual statuses, an entry may be
    /// [`DeliveryStatus::Other`]`(-1)` (the operator has not reported yet)
    /// or `Other(-2)` (unknown ID, or not sent).
    ///
    /// `POST /api/SmartSMS/GetDeliveries`
    /// (<https://www.melipayamak.com/api/getsmartsmsdeliveries/>).
    pub async fn smart_deliveries(
        &self,
        ids: &[RecId],
    ) -> Result<Vec<DeliveryStatus>, Error> {
        let op = wire::smart_deliveries(ids)?;
        let body = op.body(self.credentials());
        let (status, bytes) =
            self.post(op.path(), op.content_type(), body).await?;
        op.decode(status, &bytes)
    }
}
