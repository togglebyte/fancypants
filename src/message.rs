use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
//     - Message -
// -----------------------------------------------------------------------------
#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub payload: Vec<u8>,
    pub message_type: MessageType,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MessageType {
    Hello,
}


