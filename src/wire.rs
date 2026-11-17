// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! The wire format, without a transport: for callers that send requests
//! through an HTTP client of their own.
//!
//! Every call is an [`Operation`] constant here, typed by its request and
//! response, except [`smart_deliveries`], whose decoding depends on the
//! IDs it asks about.  [`Client`](crate::Client) sends through the same
//! code, so the two can't drift apart.  To call one yourself:
//!
//! 1. `POST` [`Operation::body`] to `base URL + `[`Operation::path`], with
//!    `Content-Type: `[`Operation::content_type`] and
//!    `Accept: application/json`.
//! 2. Read at most [`MAX_RESPONSE_BYTES`] of the response.
//! 3. Hand the status and body to [`Operation::decode`], which applies the
//!    success rule described at the crate root.
//!
//! Melipayamak takes the username and API key in the request body, not in
//! a header, so **every body carries the API key**.  Treat it as a secret:
//! send it over TLS only, and keep it out of logs, traces and error
//! messages, including your HTTP client's request dumps.
//!
//! ```
//! use melipayamak::sms::SendPattern;
//! use melipayamak::wire::{self, Credentials};
//!
//! let creds = Credentials::new("username", "api-key")?;
//! let req = SendPattern {
//!     to: "09120000000".into(),
//!     body_id: 123456,
//!     args: vec!["4321".into()],
//! };
//! let op = wire::SEND_PATTERN;
//! assert_eq!(op.path(), "/api/SendSMS/BaseServiceNumber");
//! assert_eq!(op.content_type(), "application/x-www-form-urlencoded");
//! let _body: Vec<u8> = op.body(&creds, &req)?; // secret: holds the key
//! // ... send it, then:
//! let reply = br#"{"Value":"5084123456789012345","RetStatus":1}"#;
//! let rec_id = op.decode(200, reply)?;
//! assert_eq!(rec_id.0, 5084123456789012345);
//!
//! // Later, its delivery status:
//! let _body = wire::DELIVERY.body(&creds, &rec_id)?;
//! # Ok::<(), melipayamak::Error>(())
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, Error, RetStatus};
use crate::pattern::{AddPattern, EditPattern, Pattern, PatternsRequest};
use crate::smart::{SmartSend, SmartSendMultiple, SmartSenders};
use crate::sms::{
    BasePriceRequest, MessagesRequest, SchedulePattern, SendMultiple, SendOtp,
    SendPattern, SendSms,
};
use crate::{DeliveryStatus, RecId, sms, value};

/// Melipayamak's REST endpoint.
pub const DEFAULT_BASE_URL: &str = "https://rest.payamak-panel.com";

/// Upper bound on a response body.  The largest documented payload is a
/// page of [`MESSAGES`], whose size the caller picks with `count`; this
/// leaves room for thousands of messages.
pub const MAX_RESPONSE_BYTES: usize = 8 << 20;

const FORM: &str = "application/x-www-form-urlencoded";
const JSON: &str = "application/json";

/// The panel username and its API key, which every request carries in its
/// body.
///
/// Its `Debug` output shows neither.
#[derive(Clone)]
pub struct Credentials {
    username: String,
    api_key: String,
}

impl Credentials {
    /// `api_key` is the key from the panel's settings, not the account
    /// password.  Refuses an empty username or key.
    pub fn new(
        username: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, Error> {
        let (username, api_key) = (username.into(), api_key.into());
        if username.is_empty() {
            return Err(Error::InvalidArgument("username is empty"));
        }
        if api_key.is_empty() {
            return Err(Error::InvalidArgument("API key is empty"));
        }
        Ok(Credentials { username, api_key })
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

/// One call: `POST` a `Req` to its path, get a `Resp` back.
pub struct Operation<Req, Resp> {
    path: &'static str,
    encode: Encode<Req>,
    read: fn(&Reply) -> Result<Resp, Error>,
}

/// How a request becomes a body.
enum Encode<Req> {
    /// Form fields, after the credentials.
    Form(fn(&Req, &mut Form) -> Result<(), Error>),
    /// A JSON object, credentials included.
    Json(fn(&Credentials, &Req) -> Result<Vec<u8>, Error>),
}

impl<Req> Clone for Encode<Req> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Req> Copy for Encode<Req> {}

impl<Req, Resp> Operation<Req, Resp> {
    const fn new(
        path: &'static str,
        encode: Encode<Req>,
        read: fn(&Reply) -> Result<Resp, Error>,
    ) -> Self {
        Operation { path, encode, read }
    }

    /// The path, to append to the base URL.
    pub const fn path(&self) -> &'static str {
        self.path
    }

    /// The body's `Content-Type`: `application/x-www-form-urlencoded` or
    /// `application/json`, depending on the call.
    pub const fn content_type(&self) -> &'static str {
        match self.encode {
            Encode::Form(_) => FORM,
            Encode::Json(_) => JSON,
        }
    }

    /// The request body, credentials included.  Refuses a request that
    /// would change meaning on the wire, such as a recipient with a comma.
    ///
    /// The body carries the API key: treat it as a secret, and keep it out
    /// of logs.
    pub fn body(
        &self,
        creds: &Credentials,
        req: &Req,
    ) -> Result<Vec<u8>, Error> {
        match self.encode {
            Encode::Form(fields) => {
                let mut form = Form::credentials(creds);
                fields(req, &mut form)?;
                Ok(form.into_bytes())
            }
            Encode::Json(json) => json(creds, req),
        }
    }

    /// The response, classified as the crate root describes: only a 2xx
    /// status with `RetStatus` 1 and a `Value` of the call's documented
    /// successful form is a success.
    pub fn decode(&self, status: u16, body: &[u8]) -> Result<Resp, Error> {
        (self.read)(&decode(status, body)?)
    }
}

impl<Req, Resp> Clone for Operation<Req, Resp> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Req, Resp> Copy for Operation<Req, Resp> {}

impl<Req, Resp> fmt::Debug for Operation<Req, Resp> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Operation")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

// ---- Encoding. ----

/// An `application/x-www-form-urlencoded` body, encoded as reqwest's
/// `.form()` does (through `form_urlencoded`): letters, digits and `*-._`
/// as they are, space as `+`, every other byte as `%XX`.
struct Form(String);

impl Form {
    fn credentials(creds: &Credentials) -> Self {
        let mut form = Form(String::new());
        form.push("username", &creds.username);
        form.push("password", &creds.api_key);
        form
    }

    fn push(&mut self, name: &str, value: &str) {
        if !self.0.is_empty() {
            self.0.push('&');
        }
        self.encode(name);
        self.0.push('=');
        self.encode(value);
    }

    fn encode(&mut self, s: &str) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for &b in s.as_bytes() {
            match b {
                b'*' | b'-' | b'.' | b'_' => self.0.push(char::from(b)),
                b if b.is_ascii_alphanumeric() => self.0.push(char::from(b)),
                b' ' => self.0.push('+'),
                b => {
                    self.0.push('%');
                    self.0.push(char::from(HEX[usize::from(b >> 4)]));
                    self.0.push(char::from(HEX[usize::from(b & 15)]));
                }
            }
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.0.into_bytes()
    }
}

/// Encodes a form body with the given credentials, as
/// [`Operation::body`] does.  For the test that checks it against reqwest.
#[cfg(all(test, feature = "client"))]
pub(crate) fn encode_form(
    creds: &Credentials,
    fields: &[(&str, &str)],
) -> String {
    let mut form = Form::credentials(creds);
    for (k, v) in fields {
        form.push(k, v);
    }
    form.0
}

#[derive(Serialize)]
struct Credentialed<'a, B> {
    username: &'a str,
    password: &'a str,
    #[serde(flatten)]
    body: &'a B,
}

/// `body` as a JSON object, after the credentials.
fn json<B: Serialize>(creds: &Credentials, body: &B) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&Credentialed {
        username: &creds.username,
        password: &creds.api_key,
        body,
    })
    .map_err(|_| Error::InvalidArgument("request does not serialise"))
}

/// Joins `items` with `sep`, refusing any item that contains `sep`, since
/// that would silently change the list Melipayamak sees.
fn join(
    items: &[String],
    sep: char,
    what: &'static str,
) -> Result<String, Error> {
    if items.iter().any(|s| s.contains(sep)) {
        return Err(Error::InvalidArgument(what));
    }
    Ok(items.join(&sep.to_string()))
}

fn recipients(to: &[String]) -> Result<String, Error> {
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
fn indexed<'a>(
    form: &mut Form,
    name: &str,
    items: impl IntoIterator<Item = &'a str>,
) {
    for (i, v) in items.into_iter().enumerate() {
        form.push(&format!("{name}[{i}]"), v);
    }
}

fn senders(form: &mut Form, s: &SmartSenders) {
    form.push("from", &s.from);
    form.push("fromSupportOne", &s.from_support_one);
    form.push("fromSupportTwo", &s.from_support_two);
}

fn send_sms_form(req: &SendSms, form: &mut Form) -> Result<(), Error> {
    form.push("to", &recipients(&req.to)?);
    form.push("from", &req.from);
    form.push("text", &req.text);
    form.push("isflash", &req.flash.to_string());
    Ok(())
}

fn send_otp_form(req: &SendOtp, form: &mut Form) -> Result<(), Error> {
    form.push("to", &req.to);
    form.push("from", &req.from);
    form.push("code", &req.code.to_string());
    Ok(())
}

#[derive(Serialize)]
struct SendMultipleBody<'a> {
    to: Vec<&'a str>,
    from: &'a str,
    text: Vec<&'a str>,
}

fn send_multiple_json(
    creds: &Credentials,
    req: &SendMultiple,
) -> Result<Vec<u8>, Error> {
    if req.messages.is_empty() {
        return Err(Error::InvalidArgument("no recipients"));
    }
    let body = SendMultipleBody {
        to: req.messages.iter().map(|m| m.to.as_str()).collect(),
        from: &req.from,
        text: req.messages.iter().map(|m| m.text.as_str()).collect(),
    };
    json(creds, &body)
}

fn send_pattern_form(req: &SendPattern, form: &mut Form) -> Result<(), Error> {
    form.push("text", &pattern_args(&req.args)?);
    form.push("to", &req.to);
    form.push("bodyId", &req.body_id.to_string());
    Ok(())
}

fn schedule_pattern_form(
    req: &SchedulePattern,
    form: &mut Form,
) -> Result<(), Error> {
    form.push("text", &pattern_args(&req.args)?);
    form.push("to", &req.to);
    form.push("bodyId", &req.body_id.to_string());
    form.push("scheduleDate", &req.schedule_date);
    Ok(())
}

fn delivery_form(rec_id: &RecId, form: &mut Form) -> Result<(), Error> {
    form.push("recId", &rec_id.to_string());
    Ok(())
}

fn messages_form(req: &MessagesRequest, form: &mut Form) -> Result<(), Error> {
    form.push("location", &req.location.code().to_string());
    form.push("from", req.from.as_deref().unwrap_or_default());
    form.push("index", &req.index.to_string());
    form.push("count", &req.count.to_string());
    Ok(())
}

fn no_fields(_: &(), _: &mut Form) -> Result<(), Error> {
    Ok(())
}

fn base_price_form(
    req: &BasePriceRequest,
    form: &mut Form,
) -> Result<(), Error> {
    form.push("irancellCount", &req.irancell_count.to_string());
    form.push("mtnCount", &req.mtn_count.to_string());
    form.push("from", &req.from);
    form.push("text", &req.text);
    Ok(())
}

fn smart_send_form(req: &SmartSend, form: &mut Form) -> Result<(), Error> {
    form.push("to", &recipients(&req.to)?);
    form.push("text", &req.text);
    senders(form, &req.senders);
    Ok(())
}

fn smart_send_multiple_form(
    req: &SmartSendMultiple,
    form: &mut Form,
) -> Result<(), Error> {
    if req.messages.is_empty() {
        return Err(Error::InvalidArgument("no recipients"));
    }
    indexed(form, "to", req.messages.iter().map(|m| m.to.as_str()));
    indexed(form, "text", req.messages.iter().map(|m| m.text.as_str()));
    senders(form, &req.senders);
    Ok(())
}

#[derive(Serialize)]
struct BodyIdBody {
    #[serde(rename = "bodyId")]
    body_id: i32,
}

fn delete_pattern_json(
    creds: &Credentials,
    body_id: &i32,
) -> Result<Vec<u8>, Error> {
    json(creds, &BodyIdBody { body_id: *body_id })
}

// ---- Reading `Value`, per call; see the value module. ----

fn send_multiple_value(r: &Reply) -> Result<Vec<RecId>, Error> {
    // Its docs also list a bare 1 as success.
    if r.value.as_deref().map(str::trim) == Some("1") {
        return Ok(Vec::new());
    }
    value::rec_ids(r)
}

fn patterns_value(r: &Reply) -> Result<Vec<Pattern>, Error> {
    value::one(r)?;
    value::records(r)
}

fn add_pattern_value(r: &Reply) -> Result<i32, Error> {
    let id = value::positive_id(r)?;
    i32::try_from(id).map_err(|_| r.unexpected())
}

// ---- The operations. ----

// SendSMS
/// `POST /api/SendSMS/SendSMS`: one text to one or more recipients, as
/// [`Client::send_sms`](crate::Client::send_sms).
pub const SEND_SMS: Operation<SendSms, Vec<RecId>> = Operation::new(
    "/api/SendSMS/SendSMS",
    Encode::Form(send_sms_form),
    value::rec_ids,
);
/// `POST /api/SendSMS/SendOtp`, as
/// [`Client::send_otp`](crate::Client::send_otp).
pub const SEND_OTP: Operation<SendOtp, RecId> = Operation::new(
    "/api/SendSMS/SendOtp",
    Encode::Form(send_otp_form),
    value::rec_id,
);
/// `POST /api/SendSMS/SendMultipleSMS`, JSON body, as
/// [`Client::send_multiple`](crate::Client::send_multiple).
pub const SEND_MULTIPLE: Operation<SendMultiple, Vec<RecId>> = Operation::new(
    "/api/SendSMS/SendMultipleSMS",
    Encode::Json(send_multiple_json),
    send_multiple_value,
);
/// `POST /api/SendSMS/BaseServiceNumber`: a pattern from the shared
/// service line, as [`Client::send_pattern`](crate::Client::send_pattern).
pub const SEND_PATTERN: Operation<SendPattern, RecId> = Operation::new(
    "/api/SendSMS/BaseServiceNumber",
    Encode::Form(send_pattern_form),
    value::rec_id,
);
/// `POST /api/SendSMS/BaseServiceNumberSchedule`, as
/// [`Client::schedule_pattern`](crate::Client::schedule_pattern).
pub const SCHEDULE_PATTERN: Operation<SchedulePattern, RecId> = Operation::new(
    "/api/SendSMS/BaseServiceNumberSchedule",
    Encode::Form(schedule_pattern_form),
    value::rec_id,
);
/// `POST /api/SendSMS/GetDeliveries2`: one message's delivery status, as
/// [`Client::delivery`](crate::Client::delivery).
pub const DELIVERY: Operation<RecId, DeliveryStatus> = Operation::new(
    "/api/SendSMS/GetDeliveries2",
    Encode::Form(delivery_form),
    value::delivery,
);
/// `POST /api/SendSMS/GetMessages`, as
/// [`Client::messages`](crate::Client::messages).
pub const MESSAGES: Operation<MessagesRequest, Vec<sms::Message>> =
    Operation::new(
        "/api/SendSMS/GetMessages",
        Encode::Form(messages_form),
        value::records,
    );
/// `POST /api/SendSMS/GetCredit`: the remaining credit, as
/// [`Client::credit`](crate::Client::credit).  The request is `()`.
pub const CREDIT: Operation<(), String> = Operation::new(
    "/api/SendSMS/GetCredit",
    Encode::Form(no_fields),
    value::amount,
);
/// `POST /api/SendSMS/GetBasePrice`, as
/// [`Client::base_price`](crate::Client::base_price).
pub const BASE_PRICE: Operation<BasePriceRequest, String> = Operation::new(
    "/api/SendSMS/GetBasePrice",
    Encode::Form(base_price_form),
    value::amount,
);

// SmartSMS
/// `POST /api/SmartSMS/Send`, as
/// [`Client::smart_send`](crate::Client::smart_send).
pub const SMART_SEND: Operation<SmartSend, Vec<RecId>> = Operation::new(
    "/api/SmartSMS/Send",
    Encode::Form(smart_send_form),
    value::rec_ids,
);
/// `POST /api/SmartSMS/SendMultiple`, as
/// [`Client::smart_send_multiple`](crate::Client::smart_send_multiple).
pub const SMART_SEND_MULTIPLE: Operation<SmartSendMultiple, Vec<RecId>> =
    Operation::new(
        "/api/SmartSMS/SendMultiple",
        Encode::Form(smart_send_multiple_form),
        value::rec_ids,
    );

// ShareService
/// `POST /api/ShareService/GetBodies`, JSON body, as
/// [`Client::patterns`](crate::Client::patterns).
pub const PATTERNS: Operation<PatternsRequest, Vec<Pattern>> = Operation::new(
    "/api/ShareService/GetBodies",
    Encode::Json(json::<PatternsRequest>),
    patterns_value,
);
/// `POST /api/ShareService/AddBody`, JSON body, as
/// [`Client::add_pattern`](crate::Client::add_pattern).
pub const ADD_PATTERN: Operation<AddPattern, i32> = Operation::new(
    "/api/ShareService/AddBody",
    Encode::Json(json::<AddPattern>),
    add_pattern_value,
);
/// `POST /api/ShareService/EditBody`, JSON body, as
/// [`Client::edit_pattern`](crate::Client::edit_pattern).
pub const EDIT_PATTERN: Operation<EditPattern, ()> = Operation::new(
    "/api/ShareService/EditBody",
    Encode::Json(json::<EditPattern>),
    value::one,
);
/// `POST /api/ShareService/DeleteBody`, JSON body, as
/// [`Client::delete_pattern`](crate::Client::delete_pattern).  The
/// request is the pattern's ID.
pub const DELETE_PATTERN: Operation<i32, ()> = Operation::new(
    "/api/ShareService/DeleteBody",
    Encode::Json(delete_pattern_json),
    value::one,
);

// ---- SmartSMS/GetDeliveries, whose answer is checked against its IDs. ----

/// `POST /api/SmartSMS/GetDeliveries` for given IDs, from
/// [`smart_deliveries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartDeliveries {
    ids: Vec<RecId>,
}

impl SmartDeliveries {
    /// The path, to append to the base URL.
    pub const fn path(&self) -> &'static str {
        "/api/SmartSMS/GetDeliveries"
    }

    /// The body's `Content-Type`.
    pub const fn content_type(&self) -> &'static str {
        FORM
    }

    /// The request body, credentials included.
    ///
    /// The body carries the API key: treat it as a secret, and keep it out
    /// of logs.
    pub fn body(&self, creds: &Credentials) -> Vec<u8> {
        let mut form = Form::credentials(creds);
        let ids: Vec<String> = self.ids.iter().map(RecId::to_string).collect();
        indexed(&mut form, "Ids", ids.iter().map(String::as_str));
        form.into_bytes()
    }

    /// The statuses, one per ID, in order, under the same success rule as
    /// [`Operation`].  Besides the usual statuses, an entry may be
    /// [`DeliveryStatus::Other`]`(-1)` (the operator has not reported yet)
    /// or `Other(-2)` (unknown ID, or not sent).
    pub fn decode(
        &self,
        status: u16,
        body: &[u8],
    ) -> Result<Vec<DeliveryStatus>, Error> {
        let r = decode(status, body)?;
        let list = value::int_list(&r)?;
        // Codes that fail the whole call, as opposed to one entry.
        if let [n] = list[..]
            && matches!(n, -3 | -10 | -112..=-108)
        {
            return Err(r.api_error());
        }
        if list.len() != self.ids.len() {
            return Err(r.unexpected());
        }
        Ok(list
            .into_iter()
            .map(|n| match n {
                // -1 means something else here than for DELIVERY.
                -1 => DeliveryStatus::Other(-1),
                n => DeliveryStatus::from_code(n),
            })
            .collect())
    }
}

/// `POST /api/SmartSMS/GetDeliveries`: the delivery status of Smart sends,
/// as [`Client::smart_deliveries`](crate::Client::smart_deliveries).
/// Refuses an empty list.
pub fn smart_deliveries(ids: &[RecId]) -> Result<SmartDeliveries, Error> {
    if ids.is_empty() {
        return Err(Error::InvalidArgument("no IDs"));
    }
    Ok(SmartDeliveries { ids: ids.to_vec() })
}

// ---- The envelope. ----

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
    use crate::pattern::PatternStatus;
    use crate::sms::{Location, Outgoing};

    const REC: &str = "5084123456789012345";

    fn creds() -> Credentials {
        Credentials::new("u", "k").unwrap()
    }

    fn ok(value: &str) -> Vec<u8> {
        format!(r#"{{"Value":{value},"RetStatus":1,"StrRetStatus":"Ok"}}"#)
            .into_bytes()
    }

    fn text<Req, Resp>(op: Operation<Req, Resp>, req: &Req) -> String {
        String::from_utf8(op.body(&creds(), req).unwrap()).unwrap()
    }

    fn refused<T: fmt::Debug>(r: Result<T, Error>) -> bool {
        matches!(r, Err(Error::InvalidArgument(_)))
    }

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
    fn credentials() {
        assert!(refused(Credentials::new("", "k")));
        assert!(refused(Credentials::new("u", "")));
        let c = Credentials::new("someuser", "s3cr3t-k3y").unwrap();
        for shown in [format!("{c:?}"), format!("{:?}", c.clone())] {
            assert!(
                !shown.contains("s3cr3t") && !shown.contains("someuser"),
                "{shown}"
            );
        }
    }

    #[test]
    fn form_encoding() {
        let c = Credentials::new("a b", "+&=%").unwrap();
        let mut f = Form::credentials(&c);
        f.push("to[0]", "سلام\r\n");
        f.push("x", "*-._~!'()");
        f.push("", "");
        assert_eq!(
            f.0,
            "username=a+b&password=%2B%26%3D%25\
             &to%5B0%5D=%D8%B3%D9%84%D8%A7%D9%85%0D%0A\
             &x=*-._%7E%21%27%28%29&="
        );
    }

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

    fn senders() -> SmartSenders {
        SmartSenders {
            from: "5000".into(),
            from_support_one: "5001".into(),
            from_support_two: "5002".into(),
        }
    }

    fn outgoing() -> Vec<Outgoing> {
        vec![
            Outgoing {
                to: "0912".into(),
                text: "a".into(),
            },
            Outgoing {
                to: "0935".into(),
                text: "b & c".into(),
            },
        ]
    }

    /// Every body, byte for byte, as the client sends it.
    #[test]
    fn bodies() {
        let cred = "username=u&password=k";
        // Checks the path and content type; the body expected.
        let form = |want_path: &str, path: &str, ct: &str, fields: &str| {
            assert_eq!(path, want_path);
            assert_eq!(ct, FORM, "{path}");
            if fields.is_empty() {
                cred.to_owned()
            } else {
                format!("{cred}&{fields}")
            }
        };
        let sms = SendSms {
            to: vec!["0912".into(), "0935".into()],
            from: "5000".into(),
            text: "سلام لغو11".into(),
            flash: true,
        };
        assert_eq!(
            text(SEND_SMS, &sms),
            form(
                "/api/SendSMS/SendSMS",
                SEND_SMS.path(),
                SEND_SMS.content_type(),
                "to=0912%2C0935&from=5000\
                 &text=%D8%B3%D9%84%D8%A7%D9%85+%D9%84%D8%BA%D9%8811\
                 &isflash=true"
            )
        );
        let otp = SendOtp {
            to: "0912".into(),
            from: "5000".into(),
            code: 1212,
        };
        assert_eq!(
            text(SEND_OTP, &otp),
            form(
                "/api/SendSMS/SendOtp",
                SEND_OTP.path(),
                SEND_OTP.content_type(),
                "to=0912&from=5000&code=1212"
            )
        );
        let pattern = SendPattern {
            to: "0912".into(),
            body_id: 123456,
            args: vec!["Ali".into(), "4321".into()],
        };
        assert_eq!(
            text(SEND_PATTERN, &pattern),
            form(
                "/api/SendSMS/BaseServiceNumber",
                SEND_PATTERN.path(),
                SEND_PATTERN.content_type(),
                "text=Ali%3B4321&to=0912&bodyId=123456"
            )
        );
        let scheduled = SchedulePattern {
            to: "0912".into(),
            body_id: 7,
            args: vec!["x".into()],
            schedule_date: "2024-08-04T15:20:00".into(),
        };
        assert_eq!(
            text(SCHEDULE_PATTERN, &scheduled),
            form(
                "/api/SendSMS/BaseServiceNumberSchedule",
                SCHEDULE_PATTERN.path(),
                SCHEDULE_PATTERN.content_type(),
                "text=x&to=0912&bodyId=7\
                 &scheduleDate=2024-08-04T15%3A20%3A00"
            )
        );
        assert_eq!(
            text(DELIVERY, &RecId(42)),
            form(
                "/api/SendSMS/GetDeliveries2",
                DELIVERY.path(),
                DELIVERY.content_type(),
                "recId=42"
            )
        );
        let messages = MessagesRequest {
            location: Location::All,
            from: None,
            index: 0,
            count: 50,
        };
        assert_eq!(
            text(MESSAGES, &messages),
            form(
                "/api/SendSMS/GetMessages",
                MESSAGES.path(),
                MESSAGES.content_type(),
                "location=-1&from=&index=0&count=50"
            )
        );
        assert_eq!(
            text(CREDIT, &()),
            form(
                "/api/SendSMS/GetCredit",
                CREDIT.path(),
                CREDIT.content_type(),
                ""
            )
        );
        let price = BasePriceRequest {
            irancell_count: 2,
            mtn_count: 3,
            from: "5000".into(),
            text: "t".into(),
        };
        assert_eq!(
            text(BASE_PRICE, &price),
            form(
                "/api/SendSMS/GetBasePrice",
                BASE_PRICE.path(),
                BASE_PRICE.content_type(),
                "irancellCount=2&mtnCount=3&from=5000&text=t"
            )
        );
        let smart = SmartSend {
            to: vec!["0912".into()],
            text: "t".into(),
            senders: senders(),
        };
        assert_eq!(
            text(SMART_SEND, &smart),
            form(
                "/api/SmartSMS/Send",
                SMART_SEND.path(),
                SMART_SEND.content_type(),
                "to=0912&text=t\
                 &from=5000&fromSupportOne=5001&fromSupportTwo=5002"
            )
        );
        let smart_multiple = SmartSendMultiple {
            messages: outgoing(),
            senders: senders(),
        };
        assert_eq!(
            text(SMART_SEND_MULTIPLE, &smart_multiple),
            form(
                "/api/SmartSMS/SendMultiple",
                SMART_SEND_MULTIPLE.path(),
                SMART_SEND_MULTIPLE.content_type(),
                "to%5B0%5D=0912&to%5B1%5D=0935\
                 &text%5B0%5D=a&text%5B1%5D=b+%26+c\
                 &from=5000&fromSupportOne=5001&fromSupportTwo=5002"
            )
        );
        let op = smart_deliveries(&[RecId(7), RecId(8)]).unwrap();
        assert_eq!(
            String::from_utf8(op.body(&creds())).unwrap(),
            form(
                "/api/SmartSMS/GetDeliveries",
                op.path(),
                op.content_type(),
                "Ids%5B0%5D=7&Ids%5B1%5D=8"
            )
        );

        let json = |want_path: &str, path: &str, ct: &str, fields: &str| {
            assert_eq!(path, want_path);
            assert_eq!(ct, JSON, "{path}");
            format!(r#"{{"username":"u","password":"k"{fields}}}"#)
        };
        let multiple = SendMultiple {
            from: "5000".into(),
            messages: outgoing(),
        };
        assert_eq!(
            text(SEND_MULTIPLE, &multiple),
            json(
                "/api/SendSMS/SendMultipleSMS",
                SEND_MULTIPLE.path(),
                SEND_MULTIPLE.content_type(),
                r#","to":["0912","0935"],"from":"5000","text":["a","b & c"]"#
            )
        );
        let patterns = PatternsRequest {
            keyword: Some("کد".into()),
            status: Some(PatternStatus::Approved),
        };
        assert_eq!(
            text(PATTERNS, &patterns),
            json(
                "/api/ShareService/GetBodies",
                PATTERNS.path(),
                PATTERNS.content_type(),
                r#","keyword":"کد","status":1"#
            )
        );
        assert_eq!(
            text(PATTERNS, &PatternsRequest::default()),
            json(
                "/api/ShareService/GetBodies",
                PATTERNS.path(),
                PATTERNS.content_type(),
                ""
            )
        );
        let add = AddPattern {
            title: "t".into(),
            body: "{0}".into(),
        };
        assert_eq!(
            text(ADD_PATTERN, &add),
            json(
                "/api/ShareService/AddBody",
                ADD_PATTERN.path(),
                ADD_PATTERN.content_type(),
                r#","title":"t","body":"{0}""#
            )
        );
        let edit = EditPattern {
            body_id: 4712,
            title: "t".into(),
            body: "{0}".into(),
        };
        assert_eq!(
            text(EDIT_PATTERN, &edit),
            json(
                "/api/ShareService/EditBody",
                EDIT_PATTERN.path(),
                EDIT_PATTERN.content_type(),
                r#","bodyId":4712,"title":"t","body":"{0}""#
            )
        );
        assert_eq!(
            text(DELETE_PATTERN, &4712),
            json(
                "/api/ShareService/DeleteBody",
                DELETE_PATTERN.path(),
                DELETE_PATTERN.content_type(),
                r#","bodyId":4712"#
            )
        );
    }

    /// Requests that would change meaning on the wire are never encoded.
    #[test]
    fn ambiguous_requests_are_refused() {
        let c = creds();
        let sms = |to: &[&str]| SendSms {
            to: to.iter().map(|s| s.to_string()).collect(),
            from: "5000".into(),
            text: "t".into(),
            flash: false,
        };
        assert!(refused(SEND_SMS.body(&c, &sms(&["0912,0935"]))));
        assert!(refused(SEND_SMS.body(&c, &sms(&[]))));
        let pattern = SendPattern {
            to: "0912".into(),
            body_id: 1,
            args: vec!["a;b".into()],
        };
        assert!(refused(SEND_PATTERN.body(&c, &pattern)));
        let scheduled = SchedulePattern {
            to: "0912".into(),
            body_id: 1,
            args: vec!["a;b".into()],
            schedule_date: "2024-08-04T15:20:00".into(),
        };
        assert!(refused(SCHEDULE_PATTERN.body(&c, &scheduled)));
        let multiple = SendMultiple {
            from: "5000".into(),
            messages: vec![],
        };
        assert!(refused(SEND_MULTIPLE.body(&c, &multiple)));
        let smart = SmartSend {
            to: vec!["0912,0935".into()],
            text: "t".into(),
            senders: senders(),
        };
        assert!(refused(SMART_SEND.body(&c, &smart)));
        let smart_multiple = SmartSendMultiple {
            messages: vec![],
            senders: senders(),
        };
        assert!(refused(SMART_SEND_MULTIPLE.body(&c, &smart_multiple)));
        assert!(refused(smart_deliveries(&[])));
    }

    #[test]
    fn decoding() {
        let rec = RecId(REC.parse().unwrap());
        let quoted = format!("\"{REC}\"");
        assert_eq!(SEND_PATTERN.decode(200, &ok(&quoted)).unwrap(), rec);
        assert!(matches!(
            SEND_PATTERN.decode(200, &ok("\"11\"")),
            Err(Error::Api(e)) if e.code() == Some(11)
        ));
        assert!(matches!(
            SEND_PATTERN.decode(500, &ok(&quoted)),
            Err(Error::Api(_))
        ));
        assert_eq!(
            DELIVERY.decode(200, &ok("\"1\"")).unwrap(),
            DeliveryStatus::Delivered
        );
        assert!(matches!(
            DELIVERY.decode(200, &ok("null")),
            Err(Error::Api(_))
        ));
        assert_eq!(CREDIT.decode(200, &ok("\"1234.5\"")).unwrap(), "1234.5");
        assert!(matches!(
            CREDIT.decode(200, &ok("\"NULL\"")),
            Err(Error::Api(_))
        ));
        assert_eq!(SEND_MULTIPLE.decode(200, &ok("\"1\"")).unwrap(), vec![]);
        let two = format!("\"{REC},{REC}\"");
        assert_eq!(SEND_MULTIPLE.decode(200, &ok(&two)).unwrap().len(), 2);
        assert_eq!(ADD_PATTERN.decode(200, &ok("\"4712\"")).unwrap(), 4712);
        assert!(matches!(
            ADD_PATTERN.decode(200, &ok("\"4294967296\"")),
            Err(Error::UnexpectedValue { .. })
        ));
        assert!(EDIT_PATTERN.decode(200, &ok("\"1\"")).is_ok());
        assert!(matches!(
            DELETE_PATTERN.decode(200, &ok("\"-2\"")),
            Err(Error::Api(_))
        ));
        let bodies = br#"{"MyBase":{"Value":"1","RetStatus":1},"Data":[{"BodyID":1,"Title":"t","Body":"{0}","BodyStatus":4,"Description":null,"InsertDate":"2026-11-01T09:30:00"}]}"#;
        let list = PATTERNS.decode(200, bodies).unwrap();
        assert_eq!(list[0].body_status, PatternStatus::NeedToEdit);
        let messages = br#"{"Value":"1","RetStatus":1,"Data":[{"MsgID":9}]}"#;
        assert_eq!(MESSAGES.decode(200, messages).unwrap()[0].msg_id, 9);
        assert!(format!("{SEND_PATTERN:?}").contains("BaseServiceNumber"));
    }

    #[test]
    fn smart_delivery_lists() {
        let op = smart_deliveries(&[RecId(7), RecId(8), RecId(9)]).unwrap();
        assert_eq!(
            op.decode(200, &ok("\"1,-1,-2\"")).unwrap(),
            vec![
                DeliveryStatus::Delivered,
                DeliveryStatus::Other(-1),
                DeliveryStatus::Other(-2)
            ]
        );
        // A call-level code fails the call; a list of the wrong length is
        // not read at all.
        for code in ["\"-3\"", "\"-10\"", "\"-108\"", "\"-112\""] {
            assert!(matches!(op.decode(200, &ok(code)), Err(Error::Api(_))));
        }
        assert!(matches!(
            op.decode(200, &ok("\"1,2\"")),
            Err(Error::UnexpectedValue { .. })
        ));
        // A lone -1 or -2 is an entry, not a failure of the call.
        let one = smart_deliveries(&[RecId(7)]).unwrap();
        assert_eq!(
            one.decode(200, &ok("\"-2\"")).unwrap(),
            vec![DeliveryStatus::Other(-2)]
        );
    }
}
