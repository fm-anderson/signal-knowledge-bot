mod protocol;
mod socket;

#[cfg(test)]
mod tests;

pub use protocol::parse_signal_notification;
pub use socket::{run_mock_daemon, run_socket_client};
