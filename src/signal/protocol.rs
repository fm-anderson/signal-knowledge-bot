use crate::bot::{Destination, IncomingMessage, OutgoingMessage};
use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

pub fn parse_signal_notification(raw: &str) -> Option<IncomingMessage> {
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

pub(super) fn read_signal_message(
    reader: &mut impl BufRead,
) -> io::Result<Option<IncomingMessage>> {
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

pub(super) fn write_signal_reply(
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

pub(super) fn read_send_ack(
    reader: &mut impl BufRead,
    expected_id: u64,
    pending: &mut VecDeque<IncomingMessage>,
) -> io::Result<()> {
    loop {
        let mut line = String::new();

        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Socket closed before acknowledgment",
            ));
        }

        let response: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        // preserve notifications arriving before acknowledgment.
        if response["method"] == "receive" {
            if response.pointer("/params/envelope/dataMessage").is_some() {
                let message = parse_signal_notification(&line).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Invalid Signal data message")
                })?;

                pending.push_back(message);
            }

            continue;
        }

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

        return Ok(());
    }
}
