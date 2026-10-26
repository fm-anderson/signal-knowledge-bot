use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

#[derive(Debug, PartialEq)]
enum Destination {
    Direct(String),
    Group(String),
}

struct IncomingMessage {
    sender: String,
    text: String,
    group_id: Option<String>,
}

struct OutgoingMessage {
    destination: Destination,
    text: String,
}

fn handle_message(message: &IncomingMessage) -> OutgoingMessage {
    let reply: &str = match message.text.trim().to_lowercase().as_str() {
        "ping" => "pong",
        "/help" => "Available commands: /help, ping",
        _ => "Document questions are not supported yet.",
    };

    let destination: Destination = match &message.group_id {
        Some(group_id) => Destination::Group(group_id.clone()),
        None => Destination::Direct(message.sender.clone()),
    };

    OutgoingMessage {
        destination,
        text: reply.to_string(),
    }
}

fn parse_signal_notification(raw: &str) -> Option<IncomingMessage> {
    let json: serde_json::Value = serde_json::from_str(raw).ok()?;

    if json.get("method")?.as_str()? != "receive" {
        return None;
    }

    let envelope = json.get("params")?.get("envelope")?;
    let data_message = envelope.get("dataMessage")?;

    let sender = envelope.get("sourceNumber")?.as_str()?.to_string();
    let text = data_message.get("message")?.as_str()?.to_string();

    let group_id = data_message
        .get("groupInfo")
        .and_then(|group| group.get("groupId"))
        .and_then(|id| id.as_str())
        .map(|id| id.to_string());

    Some(IncomingMessage {
        sender,
        text,
        group_id,
    })
}

fn run_socket_demo() -> io::Result<()> {
    let (mut mock_daemon, bot_stream) = UnixStream::pair()?;

    let notification = serde_json::json!({
        "method": "receive",
        "params": {
            "envelope": {
                "sourceNumber": "+15550000001",
                "dataMessage": {
                    "message": "ping"
                }
            }
        }
    });

    // simulate signal notification.
    writeln!(mock_daemon, "{notification}")?;
    let mut reader: BufReader<UnixStream> = BufReader::new(bot_stream);
    let mut line: String = String::new();
    reader.read_line(&mut line)?;

    let incoming: IncomingMessage = parse_signal_notification(&line).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid mock Signal notification",
        )
    })?;

    let outgoing: OutgoingMessage = handle_message(&incoming);

    println!("Destination: {:?}", outgoing.destination);
    println!("Reply: {}", outgoing.text);

    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args[0] == "--socket-demo" {
        if let Err(error) = run_socket_demo() {
            eprintln!("Socket demo failed: {error}");
        }
        return;
    }

    let incoming: IncomingMessage = if args[0] == "--signal-json" {
        let raw_json: String = args[1..].join(" ");

        match parse_signal_notification(&raw_json) {
            Some(message) => message,
            None => {
                eprintln!("Invalid or unsupported Signal notification");
                return;
            }
        }
    } else {
        IncomingMessage {
            sender: "test-user".to_string(),
            text: args.join(" "),
            group_id: None,
        }
    };

    let outgoing: OutgoingMessage = handle_message(&incoming);

    println!("Destination: {:?}", outgoing.destination);
    println!("Reply: {}", outgoing.text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_unknown_questions() {
        let mut incoming: IncomingMessage = IncomingMessage {
            sender: "test-user".to_string(),
            text: " PING ".to_string(),
            group_id: None,
        };
        let reply: OutgoingMessage = handle_message(&incoming);
        assert_eq!(reply.text, "pong");
        assert_eq!(
            reply.destination,
            Destination::Direct("test-user".to_string())
        );

        incoming.text = "/HELP".to_string();
        let reply: OutgoingMessage = handle_message(&incoming);
        assert_eq!(reply.text, "Available commands: /help, ping");

        incoming.text = "Can I use a government vehicle to go home?".to_string();
        let reply: OutgoingMessage = handle_message(&incoming);
        assert_eq!(reply.text, "Document questions are not supported yet.");
    }

    #[test]
    fn group_messages_reply_to_group() {
        let incoming = IncomingMessage {
            sender: "user-123".to_string(),
            text: "ping".to_string(),
            group_id: Some("group-abc".to_string()),
        };

        let reply: OutgoingMessage = handle_message(&incoming);
        assert_eq!(reply.text, "pong");
        assert_eq!(
            reply.destination,
            Destination::Group("group-abc".to_string())
        );
    }

    #[test]
    fn parses_signal_notifications() {
        let direct_json: &str = r#"{
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

        let incoming: IncomingMessage =
            parse_signal_notification(direct_json).expect("Expected a valid Signal message");

        assert_eq!(incoming.sender, "+15550000001");
        assert_eq!(incoming.text, "ping");
        assert_eq!(incoming.group_id, None);

        let reply: OutgoingMessage = handle_message(&incoming);

        assert_eq!(reply.text, "pong");
        assert_eq!(
            reply.destination,
            Destination::Direct("+15550000001".to_string())
        );

        assert!(parse_signal_notification("invalid json").is_none());
        assert!(parse_signal_notification(r#"{"method":"version"}"#).is_none());
    }
}
