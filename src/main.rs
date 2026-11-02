use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

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

    let sender = envelope
        .get("sourceNumber")
        .and_then(|value| value.as_str())
        .or_else(|| envelope.get("source").and_then(|value| value.as_str()))
        .or_else(|| envelope.get("sourceUuid").and_then(|value| value.as_str()))?
        .to_string();
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

fn read_signal_message(reader: &mut impl BufRead) -> io::Result<Option<IncomingMessage>> {
    loop {
        let mut line = String::new();

        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }

        let json: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        if json["method"] != "receive" || json.pointer("/params/envelope/dataMessage").is_none() {
            continue;
        }

        let message = parse_signal_notification(&line).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Invalid Signal data message")
        })?;

        return Ok(Some(message));
    }
}

fn write_signal_reply(
    writer: &mut impl Write,
    account: &str,
    reply: &OutgoingMessage,
    request_id: u64,
) -> io::Result<()> {
    let mut params = serde_json::json!({
        "account": account,
        "message": reply.text,
    });

    match &reply.destination {
        Destination::Direct(recipient) => {
            params["recipient"] = serde_json::json!([recipient]);
        }
        Destination::Group(group_id) => {
            params["groupId"] = serde_json::json!(group_id);
        }
    }

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "send",
        "params": params,
    });

    writeln!(writer, "{request}")
}

fn read_send_ack(reader: &mut impl BufRead, expected_id: u64) -> io::Result<()> {
    let mut line = String::new();

    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Socket closed before acknowledgment",
        ));
    }

    let response: serde_json::Value = serde_json::from_str(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    if response["jsonrpc"] != "2.0" || response["id"].as_u64() != Some(expected_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unexpected JSON-RPC response",
        ));
    }

    if let Some(error) = response.get("error") {
        return Err(io::Error::other(format!(
            "Signal daemon rejected send: {error}"
        )));
    }

    if response.get("result").is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Missing JSON-RPC result",
        ));
    }

    Ok(())
}

fn run_mock_daemon(path: &Path) -> io::Result<()> {
    let listener = UnixListener::bind(path)?;

    println!("Mock daemon listening at {}", path.display());

    let (socket, _) = listener.accept()?;
    let mut reader = BufReader::new(socket);

    for (index, text) in ["ping", "/help"].into_iter().enumerate() {
        let request_id = index as u64 + 1;
        // Simulate an incoming Signal message.
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

        writeln!(reader.get_mut(), "{notification}")?;

        // Receive the bot's JSON-RPC send request.
        let mut line = String::new();

        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Bot disconnected before sending a reply",
            ));
        }

        let request: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        if request["jsonrpc"] != "2.0"
            || request["method"] != "send"
            || request["id"].as_u64() != Some(request_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unexpected JSON-RPC request",
            ));
        }

        println!("Mock daemon received: {}", line.trim());

        let acknowledgment = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {}
        });

        writeln!(reader.get_mut(), "{acknowledgment}")?;
    }

    println!("Mock daemon completed both exchanges");

    std::fs::remove_file(path)?;

    Ok(())
}

fn run_socket_client(path: &Path) -> io::Result<()> {
    let socket = UnixStream::connect(path)?;
    let mut reader = BufReader::new(socket);
    let mut next_request_id: u64 = 1;

    loop {
        let Some(incoming) = read_signal_message(&mut reader)? else {
            println!("Daemon disconnected");
            break;
        };

        let outgoing = handle_message(&incoming);

        println!("Destination: {:?}", outgoing.destination);
        println!("Reply: {}", outgoing.text);

        write_signal_reply(reader.get_mut(), "mock-account", &outgoing, next_request_id)?;
        read_send_ack(&mut reader, next_request_id)?;
        next_request_id += 1;

        println!("Mock daemon acknowledged send");
    }

    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        eprintln!(
            "Usage: signal-knowledge-bot <message | --signal-json | --mock-daemon | --socket-client>"
        );
        return;
    }

    if args[0] == "--mock-daemon" || args[0] == "--socket-client" {
        let Some(path) = args.get(1) else {
            eprintln!("Missing socket path");
            return;
        };

        let path = Path::new(path);

        let result = if args[0] == "--mock-daemon" {
            run_mock_daemon(path)
        } else {
            run_socket_client(path)
        };

        if let Err(error) = result {
            eprintln!("Socket error: {error}");
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

        // Serialize the outgoing JSON-RPC request.
        let mut buffer = Vec::new();

        write_signal_reply(&mut buffer, "mock-account", &reply, 42).unwrap();

        let request: serde_json::Value = serde_json::from_slice(&buffer).unwrap();

        // Verify that the reply targets the group, not the sender.
        assert_eq!(request["params"]["groupId"], "group-abc");
        assert!(request["params"].get("recipient").is_none());
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

        // Send a receipt notification with no text message.
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

        // Simulate the daemon sending two messages.
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

        // Read each message separately from the same connection.
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

        assert_eq!(request["id"].as_u64(), Some(42));
        assert_eq!(request["jsonrpc"], "2.0");
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
        let mut reader = io::Cursor::new(format!("{success}\n"));

        assert!(read_send_ack(&mut reader, 1).is_ok());

        let failure = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"Send failed"}}"#;

        let mut reader = io::Cursor::new(format!("{failure}\n"));

        let error = read_send_ack(&mut reader, 1).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("Send failed"));
    }
}
