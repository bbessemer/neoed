//! JSON-RPC messages over a language server's stdio: a `Content-Length`
//! header, a blank line, then the JSON body.

use serde_json::Value;
use tokio::io::{self, AsyncBufRead};

/// The next message from `reader`, or `None` at the end of the stream.
pub async fn read(reader: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Option<Value>> {
    let _ = reader;
    todo!()
}

/// `message`, framed for writing.
pub fn frame(message: &Value) -> Vec<u8> {
    let _ = message;
    todo!()
}
