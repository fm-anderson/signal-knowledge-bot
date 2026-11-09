use super::protocol::{
    parse_signal_notification, read_send_ack, read_signal_message, write_signal_reply,
};
use crate::bot::{Destination, OutgoingMessage, handle_message};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

#[test]
fn group_messages_reply_to_group() {
    let notification = serde_json::json!({
        "method": "receive",
        "params": {
            "envelope": {
                "sourceNumber": "+15550000001",
                "dataMessage": {
                    "message": "ping",
                    "groupInfo": {
                        "groupId": "group-abc"
                    }
                }
            }
        }
    });

    let incoming = parse_signal_notification(&notification.to_string())
        .expect("Expected a valid group message");

    assert_eq!(incoming.group_id.as_deref(), Some("group-abc"));

    let reply = handle_message(&incoming);

    assert_eq!(reply.text, "pong");
    assert_eq!(
        reply.destination,
        Destination::Group("group-abc".to_string())
    );

    let mut buffer = Vec::new();
    write_signal_reply(&mut buffer, "mock-account", &reply, 42).unwrap();

    let request: serde_json::Value = serde_json::from_slice(&buffer).unwrap();

    assert_eq!(request["params"]["groupId"], "group-abc");
    assert!(request["params"].get("recipient").is_none());
}

#[test]
fn parses_signal_notifications() {
    let direct_json = r#"{
        "method": "receive",
        "params": {
            "envelope": {
                "sourceNumber": "+15550000001",
                "dataMessage": {
                    "message": "ping"
                }
            }
        }
    }"#;

    let incoming = parse_signal_notification(direct_json).expect("Expected a valid Signal message");

    assert_eq!(incoming.sender, "+15550000001");
    assert_eq!(incoming.text, "ping");
    assert_eq!(incoming.group_id, None);

    let reply = handle_message(&incoming);
    assert_eq!(reply.text, "pong");
    assert_eq!(
        reply.destination,
        Destination::Direct("+15550000001".to_string())
    );

    let uuid_json = r#"{
        "method": "receive",
        "params": {
            "envelope": {
                "sourceUuid": "sender-uuid-123",
                "dataMessage": {
                    "message": "ping"
                }
            }
        }
    }"#;

    let incoming =
        parse_signal_notification(uuid_json).expect("Expected a message with a UUID sender");

    assert_eq!(incoming.sender, "sender-uuid-123");
    assert_eq!(incoming.text, "ping");

    assert!(parse_signal_notification("invalid json").is_none());
    assert!(parse_signal_notification(r#"{"method":"version"}"#).is_none());
}

#[test]
fn socket_reads_separate_json_messages() {
    let (mut daemon, bot) = UnixStream::pair().unwrap();

    let receipt = serde_json::json!({
        "method": "receive",
        "params": {
            "envelope": {
                "sourceNumber": "+15550000001",
                "receiptMessage": {
                    "isDelivery": true
                }
            }
        }
    });

    writeln!(daemon, "{receipt}").unwrap();

    for text in ["ping", "/help"] {
        let notification = serde_json::json!({
            "method": "receive",
            "params": {
                "envelope": {
                    "sourceNumber": "+15550000001",
                    "dataMessage": {
                        "message": text
                    }
                }
            }
        });

        writeln!(daemon, "{notification}").unwrap();
    }

    drop(daemon);

    let mut reader = BufReader::new(bot);

    let first = read_signal_message(&mut reader).unwrap().unwrap();
    let second = read_signal_message(&mut reader).unwrap().unwrap();

    assert_eq!(first.text, "ping");
    assert_eq!(second.text, "/help");

    assert_eq!(handle_message(&first).text, "pong");
    assert_eq!(
        handle_message(&second).text,
        "Available commands: /help, ping"
    );

    assert!(read_signal_message(&mut reader).unwrap().is_none());
}

#[test]
fn sends_signal_reply_over_socket() {
    let (mut bot, daemon) = UnixStream::pair().unwrap();

    let reply = OutgoingMessage {
        destination: Destination::Direct("+15550000001".to_string()),
        text: "pong".to_string(),
    };

    write_signal_reply(&mut bot, "mock-account", &reply, 42).unwrap();

    let mut reader = BufReader::new(daemon);
    let mut line = String::new();

    reader.read_line(&mut line).unwrap();

    let request: serde_json::Value = serde_json::from_str(&line).unwrap();

    assert_eq!(request["jsonrpc"], "2.0");
    assert_eq!(request["id"].as_u64(), Some(42));
    assert_eq!(request["method"], "send");
    assert_eq!(request["params"]["account"], "mock-account");
    assert_eq!(request["params"]["message"], "pong");
    assert_eq!(
        request["params"]["recipient"],
        serde_json::json!(["+15550000001"])
    );
    assert!(request["params"].get("groupId").is_none());
}

#[test]
fn validates_send_acknowledgments() {
    let success = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
    let mut pending = VecDeque::new();

    let mut reader = io::Cursor::new(format!("{success}\n"));
    assert!(read_send_ack(&mut reader, 1, &mut pending).is_ok());

    let failure = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"Send failed"}}"#;
    let mut reader = io::Cursor::new(format!("{failure}\n"));

    let error = read_send_ack(&mut reader, 1, &mut pending).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error.to_string().contains("Send failed"));

    let notification = serde_json::json!({
        "method": "receive",
        "params": {
            "envelope": {
                "sourceNumber": "+15550000002",
                "dataMessage": {
                    "message": "/help"
                }
            }
        }
    });

    let input = format!("{notification}\n{success}\n");
    let mut reader = io::Cursor::new(input);

    read_send_ack(&mut reader, 1, &mut pending).unwrap();

    let message = pending.pop_front().expect("Expected queued message");

    assert_eq!(message.sender, "+15550000002");
    assert_eq!(message.text, "/help");
    assert!(pending.is_empty());
}
