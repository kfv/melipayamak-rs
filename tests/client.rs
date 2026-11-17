// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Wire format of every call, checked against a loopback server, plus
//! transport behaviour: limits, timeouts and headers.

use std::time::Duration;

use serde_json::{Value, json};

use melipayamak::pattern::{
    AddPattern, EditPattern, PatternStatus, PatternsRequest,
};
use melipayamak::smart::{SmartSend, SmartSendMultiple, SmartSenders};
use melipayamak::sms::{
    BasePriceRequest, Location, MessagesRequest, Outgoing, SchedulePattern,
    SendMultiple, SendOtp, SendPattern, SendSms,
};
use melipayamak::wire::{self, Credentials};
use melipayamak::{
    Client, DeliveryStatus, Error, MAX_RESPONSE_BYTES, RecId, RetStatus,
};

mod common;

use common::{Captured, Reply, serve, serve_once};

const USER: &str = "test-user";
const KEY: &str = "test-api-key";
const REC: &str = "5084123456789012345";

fn client(base: &str) -> Client {
    Client::builder(USER, KEY).base_url(base).build().unwrap()
}

fn ok(value: Value) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"Value": value, "RetStatus": 1, "StrRetStatus": "Ok"}),
    )
    .unwrap()
}

/// A base URL nothing listens on: a request that does go out fails as a
/// transport error.
fn dead_base() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    base
}

fn form(got: &Captured) -> Vec<(String, String)> {
    assert_eq!(
        got.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    let body = String::from_utf8(got.body.clone()).unwrap();
    melipayamak::reqwest::Url::parse(&format!("http://x/?{body}"))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Checks a form request: path, credentials first, then `fields` in order.
fn assert_form(got: &Captured, path: &str, fields: &[(&str, &str)]) {
    assert_eq!((got.method.as_str(), got.target.as_str()), ("POST", path));
    let mut want = vec![("username", USER), ("password", KEY)];
    want.extend_from_slice(fields);
    let got = form(got);
    let got: Vec<(&str, &str)> =
        got.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(got, want);
}

/// Checks a JSON request: path and the whole object, credentials included.
fn assert_json(got: &Captured, path: &str, mut want: Value) {
    assert_eq!((got.method.as_str(), got.target.as_str()), ("POST", path));
    assert_eq!(got.header("content-type"), Some("application/json"));
    want["username"] = USER.into();
    want["password"] = KEY.into();
    let body: Value = serde_json::from_slice(&got.body).unwrap();
    assert_eq!(body, want);
}

/// The client sent exactly what the wire layer builds.
fn sent_as_wire(got: &Captured, body: Result<Vec<u8>, Error>) {
    assert_eq!(got.body, body.unwrap());
}

fn creds() -> Credentials {
    Credentials::new(USER, KEY).unwrap()
}

fn sms(to: &[&str]) -> SendSms {
    SendSms {
        to: to.iter().map(|s| s.to_string()).collect(),
        from: "50001234".into(),
        text: "سلام لغو11".into(),
        flash: false,
    }
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
            to: "09120000001".into(),
            text: "a".into(),
        },
        Outgoing {
            to: "09120000002".into(),
            text: "b & c".into(),
        },
    ]
}

#[tokio::test]
async fn send_sms_wire() {
    let (base, server) = serve_once(200, ok(json!(REC))).await;
    let ids = client(&base)
        .send_sms(&SendSms {
            flash: true,
            ..sms(&["09120000001", "09120000002"])
        })
        .await
        .unwrap();
    assert_eq!(ids, vec![RecId(REC.parse().unwrap())]);
    let got = server.await.unwrap();
    assert_form(
        &got,
        "/api/SendSMS/SendSMS",
        &[
            ("to", "09120000001,09120000002"),
            ("from", "50001234"),
            ("text", "سلام لغو11"),
            ("isflash", "true"),
        ],
    );
    assert_eq!(got.header("accept"), Some("application/json"));
    let ua = format!("melipayamak/{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(got.header("user-agent"), Some(ua.as_str()));
}

#[tokio::test]
async fn send_otp_wire() {
    let (base, server) = serve_once(200, ok(json!(REC))).await;
    let id = client(&base)
        .send_otp(&SendOtp {
            to: "09120000001".into(),
            from: "5000".into(),
            code: 1212,
        })
        .await
        .unwrap();
    assert_eq!(id.to_string(), REC);
    assert_form(
        &server.await.unwrap(),
        "/api/SendSMS/SendOtp",
        &[("to", "09120000001"), ("from", "5000"), ("code", "1212")],
    );
}

#[tokio::test]
async fn send_multiple_wire() {
    for (value, want) in [
        (json!(format!("{REC},{REC}")), 2),
        // Its docs also list a bare 1 as success.
        (json!("1"), 0),
    ] {
        let (base, server) = serve_once(200, ok(value)).await;
        let ids = client(&base)
            .send_multiple(&SendMultiple {
                from: "5000".into(),
                messages: outgoing(),
            })
            .await
            .unwrap();
        assert_eq!(ids.len(), want);
        assert_json(
            &server.await.unwrap(),
            "/api/SendSMS/SendMultipleSMS",
            json!({
                "to": ["09120000001", "09120000002"],
                "from": "5000",
                "text": ["a", "b & c"],
            }),
        );
    }
}

#[tokio::test]
async fn pattern_sends_wire() {
    let req = SendPattern {
        to: "09120000001".into(),
        body_id: 123456,
        args: vec!["Ali".into(), "4321".into()],
    };
    let (base, server) = serve_once(200, ok(json!(REC))).await;
    client(&base).send_pattern(&req).await.unwrap();
    let got = server.await.unwrap();
    assert_form(
        &got,
        "/api/SendSMS/BaseServiceNumber",
        &[
            ("text", "Ali;4321"),
            ("to", "09120000001"),
            ("bodyId", "123456"),
        ],
    );
    sent_as_wire(&got, wire::SEND_PATTERN.body(&creds(), &req));

    let (base, server) = serve_once(200, ok(json!(REC))).await;
    client(&base)
        .schedule_pattern(&SchedulePattern {
            to: "09120000001".into(),
            body_id: 7,
            args: vec!["x".into()],
            schedule_date: "2024-08-04T15:20:00".into(),
        })
        .await
        .unwrap();
    assert_form(
        &server.await.unwrap(),
        "/api/SendSMS/BaseServiceNumberSchedule",
        &[
            ("text", "x"),
            ("to", "09120000001"),
            ("bodyId", "7"),
            ("scheduleDate", "2024-08-04T15:20:00"),
        ],
    );
}

#[tokio::test]
async fn delivery_wire() {
    let (base, server) = serve_once(200, ok(json!("1"))).await;
    let s = client(&base).delivery(RecId(42)).await.unwrap();
    assert_eq!(s, DeliveryStatus::Delivered);
    let got = server.await.unwrap();
    assert_form(&got, "/api/SendSMS/GetDeliveries2", &[("recId", "42")]);
    sent_as_wire(&got, wire::DELIVERY.body(&creds(), &RecId(42)));
}

#[tokio::test]
async fn messages_wire() {
    let body = json!({
        "Value": "1",
        "RetStatus": 1,
        "StrRetStatus": "Ok",
        "Data": [{
            "MsgID": 991,
            "Body": "hi",
            "SendDate": "2026-11-09T10:00:00",
            "Sender": "0912",
            "Receiver": "5000",
            "IsRead": false,
            "Credit": 1.5,
            "Unlisted": "kept out"
        }]
    });
    let (base, server) =
        serve_once(200, serde_json::to_vec(&body).unwrap()).await;
    let msgs = client(&base)
        .messages(&MessagesRequest {
            location: Location::Received,
            from: None,
            index: 0,
            count: 50,
        })
        .await
        .unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].msg_id, 991);
    assert_eq!(msgs[0].body.as_deref(), Some("hi"));
    assert_eq!(msgs[0].credit, Some(1.5));
    assert_eq!(msgs[0].parts, None);
    assert_form(
        &server.await.unwrap(),
        "/api/SendSMS/GetMessages",
        &[
            ("location", "1"),
            ("from", ""),
            ("index", "0"),
            ("count", "50"),
        ],
    );
}

#[tokio::test]
async fn credit_and_price_wire() {
    let (base, server) = serve_once(200, ok(json!("1234.565"))).await;
    assert_eq!(client(&base).credit().await.unwrap(), "1234.565");
    let got = server.await.unwrap();
    assert_form(&got, "/api/SendSMS/GetCredit", &[]);
    sent_as_wire(&got, wire::CREDIT.body(&creds(), &()));

    let (base, server) = serve_once(200, ok(json!("950"))).await;
    let price = client(&base)
        .base_price(&BasePriceRequest {
            irancell_count: 2,
            mtn_count: 3,
            from: "5000".into(),
            text: "t".into(),
        })
        .await
        .unwrap();
    assert_eq!(price, "950");
    assert_form(
        &server.await.unwrap(),
        "/api/SendSMS/GetBasePrice",
        &[
            ("irancellCount", "2"),
            ("mtnCount", "3"),
            ("from", "5000"),
            ("text", "t"),
        ],
    );
}

#[tokio::test]
async fn smart_wire() {
    let (base, server) = serve_once(200, ok(json!(REC))).await;
    client(&base)
        .smart_send(&SmartSend {
            to: vec!["09120000001".into()],
            text: "t".into(),
            senders: senders(),
        })
        .await
        .unwrap();
    assert_form(
        &server.await.unwrap(),
        "/api/SmartSMS/Send",
        &[
            ("to", "09120000001"),
            ("text", "t"),
            ("from", "5000"),
            ("fromSupportOne", "5001"),
            ("fromSupportTwo", "5002"),
        ],
    );

    let (base, server) =
        serve_once(200, ok(json!(format!("{REC},{REC}")))).await;
    let ids = client(&base)
        .smart_send_multiple(&SmartSendMultiple {
            messages: outgoing(),
            senders: senders(),
        })
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_form(
        &server.await.unwrap(),
        "/api/SmartSMS/SendMultiple",
        &[
            ("to[0]", "09120000001"),
            ("to[1]", "09120000002"),
            ("text[0]", "a"),
            ("text[1]", "b & c"),
            ("from", "5000"),
            ("fromSupportOne", "5001"),
            ("fromSupportTwo", "5002"),
        ],
    );
}

#[tokio::test]
async fn smart_deliveries_wire() {
    let (base, server) = serve_once(200, ok(json!("1,-1,-2"))).await;
    let s = client(&base)
        .smart_deliveries(&[RecId(7), RecId(8), RecId(9)])
        .await
        .unwrap();
    assert_eq!(
        s,
        vec![
            DeliveryStatus::Delivered,
            DeliveryStatus::Other(-1),
            DeliveryStatus::Other(-2)
        ]
    );
    assert_form(
        &server.await.unwrap(),
        "/api/SmartSMS/GetDeliveries",
        &[("Ids[0]", "7"), ("Ids[1]", "8"), ("Ids[2]", "9")],
    );

    // A call-level code fails the call; a list of the wrong length is not
    // read at all.
    for (value, api) in [("-3", true), ("1,2", false)] {
        let (base, server) = serve_once(200, ok(json!(value))).await;
        let err = client(&base)
            .smart_deliveries(&[RecId(7), RecId(8), RecId(9)])
            .await
            .unwrap_err();
        if api {
            assert!(matches!(err, Error::Api(_)), "{err:?}");
        } else {
            assert!(matches!(err, Error::UnexpectedValue { .. }), "{err:?}");
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn patterns_wire() {
    let body = json!({
        "MyBase": {"Value": "1", "RetStatus": 1, "StrRetStatus": "Ok"},
        "Data": [{
            "BodyID": 4711,
            "Title": "سفارش",
            "Body": "کد {0}",
            "BodyStatus": 1,
            "Description": null,
            "InsertDate": "2026-11-01T09:30:00"
        }]
    });
    let (base, server) =
        serve_once(200, serde_json::to_vec(&body).unwrap()).await;
    let list = client(&base)
        .patterns(&PatternsRequest {
            keyword: Some("سفارش".into()),
            status: None,
        })
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].body_id, 4711);
    assert_eq!(list[0].body_status, PatternStatus::Approved);
    assert_json(
        &server.await.unwrap(),
        "/api/ShareService/GetBodies",
        json!({"keyword": "سفارش"}),
    );

    let (base, server) = serve_once(200, ok(json!("4712"))).await;
    let id = client(&base)
        .add_pattern(&AddPattern {
            title: "t".into(),
            body: "{0}".into(),
        })
        .await
        .unwrap();
    assert_eq!(id, 4712);
    assert_json(
        &server.await.unwrap(),
        "/api/ShareService/AddBody",
        json!({"title": "t", "body": "{0}"}),
    );

    let (base, server) = serve_once(200, ok(json!("1"))).await;
    client(&base)
        .edit_pattern(&EditPattern {
            body_id: 4712,
            title: "t".into(),
            body: "{0}".into(),
        })
        .await
        .unwrap();
    assert_json(
        &server.await.unwrap(),
        "/api/ShareService/EditBody",
        json!({"bodyId": 4712, "title": "t", "body": "{0}"}),
    );

    let (base, server) = serve_once(200, ok(json!("1"))).await;
    client(&base).delete_pattern(4712).await.unwrap();
    let got = server.await.unwrap();
    assert_json(
        &got,
        "/api/ShareService/DeleteBody",
        json!({"bodyId": 4712}),
    );
    sent_as_wire(&got, wire::DELETE_PATTERN.body(&creds(), &4712));
}

/// The documented AddBody failure for a missing permission.
#[tokio::test]
async fn documented_failure() {
    let body =
        json!({"Value": "-1", "RetStatus": 36, "StrRetStatus": "AccessDenied"});
    let (base, server) =
        serve_once(200, serde_json::to_vec(&body).unwrap()).await;
    let err = client(&base)
        .add_pattern(&AddPattern {
            title: "t".into(),
            body: "{0}".into(),
        })
        .await
        .unwrap_err();
    let Error::Api(e) = err else {
        panic!("expected Api error, got {err:?}")
    };
    assert_eq!(e.ret_status, Some(RetStatus::AccessDenied));
    assert_eq!(e.code(), Some(-1));
    server.await.unwrap();
}

/// A failure code under `RetStatus` 1 is still a failure.
#[tokio::test]
async fn code_under_ok_status_fails() {
    let (base, server) = serve_once(200, ok(json!("2"))).await;
    let err = client(&base).send_sms(&sms(&["0912"])).await.unwrap_err();
    let Error::Api(e) = err else {
        panic!("expected Api error, got {err:?}")
    };
    assert_eq!(e.code(), Some(2));
    server.await.unwrap();
}

/// Arguments that would change meaning once joined never leave the client.
#[tokio::test]
async fn ambiguous_arguments_are_not_sent() {
    let c = client(&dead_base());
    let errs = [
        c.send_sms(&sms(&["0912,0935"])).await.unwrap_err(),
        c.send_sms(&sms(&[])).await.unwrap_err(),
        c.send_pattern(&SendPattern {
            to: "0912".into(),
            body_id: 1,
            args: vec!["a;b".into()],
        })
        .await
        .unwrap_err(),
        c.send_multiple(&SendMultiple {
            from: "5000".into(),
            messages: vec![],
        })
        .await
        .unwrap_err(),
        c.smart_deliveries(&[]).await.unwrap_err(),
    ];
    for err in errs {
        assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    }
}

/// A body of exactly the cap is read; one byte more is refused.
#[tokio::test]
async fn response_size_is_capped() {
    let padded = |len: usize| {
        let head = br#"{"Value":"1","RetStatus":1,"StrRetStatus":"Ok","Pad":""#;
        let tail = br#""}"#;
        let mut body = head.to_vec();
        body.resize(len - tail.len(), b'x');
        body.extend_from_slice(tail);
        body
    };
    let (base, server) = serve_once(200, padded(MAX_RESPONSE_BYTES)).await;
    client(&base).delete_pattern(1).await.unwrap();
    server.await.unwrap();

    let (base, server) = serve_once(200, padded(MAX_RESPONSE_BYTES + 1)).await;
    let err = client(&base).delete_pattern(1).await.unwrap_err();
    assert!(matches!(err, Error::ResponseTooLarge), "{err:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn timeout_is_a_transport_error() {
    let (base, server) = serve(Reply::Hang).await;
    let c = Client::builder(USER, KEY)
        .base_url(base)
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    match c.credit().await {
        Err(Error::Transport(e)) => assert!(e.is_timeout(), "{e:?}"),
        other => panic!("expected a timeout, got {other:?}"),
    }
    server.await.unwrap();
}

#[tokio::test]
async fn connection_failure_is_a_transport_error() {
    let err = client(&dead_base()).credit().await.unwrap_err();
    assert!(matches!(err, Error::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn uses_the_given_http_client() {
    let mut headers = melipayamak::reqwest::header::HeaderMap::new();
    headers.insert("x-test", "yes".parse().unwrap());
    let http = melipayamak::reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let (base, server) = serve_once(200, ok(json!("10"))).await;
    let c = Client::builder(USER, KEY)
        .base_url(base)
        .http_client(http)
        .build()
        .unwrap();
    c.credit().await.unwrap();
    let got = server.await.unwrap();
    assert_eq!(got.header("x-test"), Some("yes"));
    assert_form(&got, "/api/SendSMS/GetCredit", &[]);
}
