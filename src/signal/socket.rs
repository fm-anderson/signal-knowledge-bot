use super::protocol::{read_send_ack, read_signal_message, write_signal_reply};
use crate::bot::handle_message;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

pub fn run_mock_daemon(path: &Path) -> io::Result<()> {
    let listener = UnixListener::bind(path)?;

    println!("Mock daemon listening at {}", path.display());

    let (socket, _) = listener.accept()?;
    let mut reader = BufReader::new(socket);

    // send notifications before acknowledging reply.
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

        writeln!(reader.get_mut(), "{notification}")?;
    }

    println!("Mock daemon sent both notifications");

    for (index, expected_message) in ["pong", "Available commands: /help, ping"]
        .into_iter()
        .enumerate()
    {
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
            || request["params"]["message"] != expected_message
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

pub fn run_socket_client(path: &Path) -> io::Result<()> {
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

        let outgoing = handle_message(&incoming);

        println!("Destination: {:?}", outgoing.destination);
        println!("Reply: {}", outgoing.text);

        write_signal_reply(reader.get_mut(), "mock-account", &outgoing, next_request_id)?;
        read_send_ack(&mut reader, next_request_id, &mut pending)?;

        println!("Mock daemon acknowledged send");
        next_request_id += 1;
    }

    Ok(())
}
