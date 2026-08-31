//! Defines commands accepted by a Conversation runtime.

use crate::cursor::protocol::proto::agent::v1 as pb;

#[derive(Debug)]
pub enum TransportCommand {
    Append {
        seqno: i64,
        message: Box<pb::AgentClientMessage>,
    },
    Disconnect,
    Close,
}
