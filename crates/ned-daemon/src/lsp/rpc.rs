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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::BufReader;

    use super::*;

    #[test]
    fn frames_have_a_content_length() {
        let framed = frame(&json!({"id": 1}));
        assert_eq!(
            String::from_utf8(framed).unwrap(),
            "Content-Length: 8\r\n\r\n{\"id\":1}"
        );
    }

    #[tokio::test]
    async fn reads_messages_until_the_end() {
        let mut bytes = frame(&json!({"id": 1, "result": "é"}));
        bytes.extend(frame(&json!({"method": "exit"})));
        let mut reader = BufReader::new(&bytes[..]);
        assert_eq!(
            read(&mut reader).await.unwrap(),
            Some(json!({"id": 1, "result": "é"}))
        );
        assert_eq!(
            read(&mut reader).await.unwrap(),
            Some(json!({"method": "exit"}))
        );
        assert_eq!(read(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn headers_are_case_insensitive_and_others_are_ignored() {
        let bytes = b"content-length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}";
        let mut reader = BufReader::new(&bytes[..]);
        assert_eq!(read(&mut reader).await.unwrap(), Some(json!({})));
    }

    #[tokio::test]
    async fn a_message_without_a_length_is_an_error() {
        let mut reader = BufReader::new(&b"Content-Type: x\r\n\r\n{}"[..]);
        let err = read(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_truncated_message_is_an_error() {
        let mut reader = BufReader::new(&b"Content-Length: 10\r\n\r\n{}"[..]);
        assert!(read(&mut reader).await.is_err());
    }
}
