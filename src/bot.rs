use std::io;

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
    handle_message_with_answer(message, |_| {
        Ok("Document questions are not supported yet.".to_string())
    })
}

pub fn handle_message_with_answer(
    message: &IncomingMessage,
    answer: impl FnOnce(&str) -> io::Result<String>,
) -> OutgoingMessage {
    let question = message.text.trim();

    let text = match question.to_lowercase().as_str() {
        "ping" => "pong".to_string(),
        "/help" => "Available commands: /help, ping".to_string(),
        "" => "Please send a question or use /help.".to_string(),
        _ => match answer(question) {
            Ok(response) if !response.trim().is_empty() => response,
            Ok(_) => {
                eprintln!("Document answering returned an empty response.");
                "I couldn't answer your question right now. Please try again later.".to_string()
            }
            Err(error) => {
                eprintln!("Document answering failed: {error}");
                "I couldn't answer your question right now. Please try again later.".to_string()
            }
        },
    };

    let destination = match &message.group_id {
        Some(group_id) => Destination::Group(group_id.clone()),
        None => Destination::Direct(message.sender.clone()),
    };

    OutgoingMessage { destination, text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_empty_messages_skip_answering() {
        for (text, expected) in [
            (" PING ", "pong"),
            ("/HELP", "Available commands: /help, ping"),
            ("   ", "Please send a question or use /help."),
        ] {
            let incoming = IncomingMessage {
                sender: "test-user".to_string(),
                text: text.to_string(),
                group_id: None,
            };

            let reply = handle_message_with_answer(&incoming, |_| {
                panic!("Commands and empty messages must not call the model");
            });

            assert_eq!(reply.text, expected);
            assert_eq!(
                reply.destination,
                Destination::Direct("test-user".to_string())
            );
        }
    }

    #[test]
    fn questions_use_answer_callback_and_preserve_destination() {
        for group_id in [None, Some("group-abc".to_string())] {
            let incoming = IncomingMessage {
                sender: "test-user".to_string(),
                text: "  What is SF 91?  ".to_string(),
                group_id,
            };

            let reply = handle_message_with_answer(&incoming, |question| {
                assert_eq!(question, "What is SF 91?");
                Ok("Answer from supplied sources.".to_string())
            });

            assert_eq!(reply.text, "Answer from supplied sources.");

            let expected_destination = match &incoming.group_id {
                Some(group_id) => Destination::Group(group_id.clone()),
                None => Destination::Direct("test-user".to_string()),
            };

            assert_eq!(reply.destination, expected_destination);
        }
    }

    #[test]
    fn failed_or_empty_answers_produce_a_fallback() {
        let incoming = IncomingMessage {
            sender: "test-user".to_string(),
            text: "What is SF 91?".to_string(),
            group_id: Some("group-abc".to_string()),
        };

        for outcome in [
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Model request timed out",
            )),
            Ok("   ".to_string()),
        ] {
            let reply = handle_message_with_answer(&incoming, |_| outcome);

            assert_eq!(
                reply.text,
                "I couldn't answer your question right now. Please try again later."
            );
            assert_eq!(
                reply.destination,
                Destination::Group("group-abc".to_string())
            );
        }
    }
}
