#[derive(Debug, PartialEq)]
pub enum Destination {
    Direct(String),
    Group(String),
}

pub struct IncomingMessage {
    pub sender: String,
    pub text: String,
    pub group_id: Option<String>,
}

pub struct OutgoingMessage {
    pub destination: Destination,
    pub text: String,
}

pub fn handle_message(message: &IncomingMessage) -> OutgoingMessage {
    let reply = match message.text.trim().to_lowercase().as_str() {
        "ping" => "pong",
        "/help" => "Available commands: /help, ping",
        _ => "Document questions are not supported yet.",
    };

    let destination = match &message.group_id {
        Some(group_id) => Destination::Group(group_id.clone()),
        None => Destination::Direct(message.sender.clone()),
    };

    OutgoingMessage {
        destination,
        text: reply.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_unknown_questions() {
        let mut incoming = IncomingMessage {
            sender: "test-user".to_string(),
            text: " PING ".to_string(),
            group_id: None,
        };

        let reply = handle_message(&incoming);
        assert_eq!(reply.text, "pong");
        assert_eq!(
            reply.destination,
            Destination::Direct("test-user".to_string())
        );

        incoming.text = "/HELP".to_string();
        let reply = handle_message(&incoming);
        assert_eq!(reply.text, "Available commands: /help, ping");

        incoming.text = "Can I use a government vehicle to go home?".to_string();
        let reply = handle_message(&incoming);
        assert_eq!(reply.text, "Document questions are not supported yet.");
    }
}
