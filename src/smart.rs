// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Smart sends, which retry from up to two backup numbers
//! (`/api/SmartSMS/…`).

use crate::sms::{Outgoing, indexed, recipients};
use crate::{Client, DeliveryStatus, Error, RecId, value};

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

fn senders(s: &SmartSenders) -> [(String, String); 3] {
    [
        ("from".to_owned(), s.from.clone()),
        ("fromSupportOne".to_owned(), s.from_support_one.clone()),
        ("fromSupportTwo".to_owned(), s.from_support_two.clone()),
    ]
}

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
        let mut fields = vec![
            ("to".to_owned(), recipients(&req.to)?),
            ("text".to_owned(), req.text.clone()),
        ];
        fields.extend(senders(&req.senders));
        let r = self.post_form("api/SmartSMS/Send", fields).await?;
        value::rec_ids(&r)
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
        if req.messages.is_empty() {
            return Err(Error::InvalidArgument("no recipients"));
        }
        let mut fields =
            indexed("to", req.messages.iter().map(|m| m.to.as_str()));
        fields.extend(indexed(
            "text",
            req.messages.iter().map(|m| m.text.as_str()),
        ));
        fields.extend(senders(&req.senders));
        let r = self.post_form("api/SmartSMS/SendMultiple", fields).await?;
        value::rec_ids(&r)
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
        if ids.is_empty() {
            return Err(Error::InvalidArgument("no IDs"));
        }
        let ids: Vec<String> = ids.iter().map(RecId::to_string).collect();
        let fields = indexed("Ids", ids.iter().map(String::as_str));
        let r = self.post_form("api/SmartSMS/GetDeliveries", fields).await?;
        let list = value::int_list(&r)?;
        // Codes that fail the whole call, as opposed to one entry.
        if let [n] = list[..]
            && matches!(n, -3 | -10 | -112..=-108)
        {
            return Err(r.api_error());
        }
        if list.len() != ids.len() {
            return Err(r.unexpected());
        }
        Ok(list
            .into_iter()
            .map(|n| match n {
                // -1 means something else here than for delivery().
                -1 => DeliveryStatus::Other(-1),
                n => DeliveryStatus::from_code(n),
            })
            .collect())
    }
}
