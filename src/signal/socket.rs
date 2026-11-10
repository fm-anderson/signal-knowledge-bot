use super::protocol::{read_send_ack, read_signal_message, write_signal_reply};
use crate::bot::{IncomingMessage, OutgoingMessage};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

pub fn run_mock_daemon(path: &Path) -> io::Result<()> {
    let listener = UnixListener::bind(path)?;

    println!("Mock daemon listening at {}", path.display());

    let (socket, _) = listener.accept()?;
    let mut reader = BufReader::new(socket);

    let exchanges = [
        ("ping", Some("pong")),
        ("What does the DAF use to move people and equipment?", None),
        ("/help", Some("Available commands: /help, ping")),
    ];

    // send notifications before acknowledging replies.
    for (text, _) in &exchanges {
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
    }

    println!("Mock daemon sent {} notifications", exchanges.len());

    for (index, (_, expected_message)) in exchanges.iter().enumerate() {
        let request_id = index as u64 + 1;
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
            || request["params"]["account"] != "mock-account"
            || request["params"]["recipient"] != serde_json::json!(["+15550000001"])
            || request["params"].get("groupId").is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unexpected JSON-RPC request or destination",
            ));
        }

        let reply = request["params"]["message"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing or empty reply"))?;

        if let Some(expected) = expected_message
            && reply != *expected
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unexpected command reply",
            ));
        }

        println!("\nMock daemon received reply {request_id}:\n{reply}");

        let acknowledgment = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {}
        });

        writeln!(reader.get_mut(), "{acknowledgment}")?;
    }

    println!("\nMock daemon completed all three exchanges");
    std::fs::remove_file(path)?;
    Ok(())
}

pub fn run_socket_client(
    path: &Path,
    mut handle: impl FnMut(&IncomingMessage) -> OutgoingMessage,
) -> io::Result<()> {
    let socket = UnixStream::connect(path)?;
    let mut reader = BufReader::new(socket);

    let mut next_request_id: u64 = 1;
    let mut pending = VecDeque::new();

    loop {
        let incoming = if let Some(message) = pending.pop_front() {
            message
        } else {
            let Some(message) = read_signal_message(&mut reader)? else {
                println!("Daemon disconnected");
                break;
            };

            message
        };

        let outgoing = handle(&incoming);

        println!("Destination: {:?}", outgoing.destination);
        println!("Reply: {}", outgoing.text);

        write_signal_reply(reader.get_mut(), "mock-account", &outgoing, next_request_id)?;
        read_send_ack(&mut reader, next_request_id, &mut pending)?;

        println!("Mock daemon acknowledged send");
        next_request_id += 1;
    }

    Ok(())
}
