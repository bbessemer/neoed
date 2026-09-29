//! JSON-RPC messages over a language server's stdio: a `Content-Length`
//! header, a blank line, then the JSON body.

use serde_json::Value;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// The next message from `reader`, or `None` at the end of the stream.
pub async fn read(reader: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Option<Value>> {
    let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut length = None;
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header).await? == 0 {
            return match length {
                None => Ok(None),
                Some(_) => Err(io::ErrorKind::UnexpectedEof.into()),
            };
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            let value = value.trim();
            length = Some(
                value
                    .parse()
                    .map_err(|_| invalid(format!("bad Content-Length `{value}`")))?,
            );
        }
    }
    let length = length.ok_or_else(|| invalid("a message without Content-Length".into()))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|err| invalid(err.to_string()))
}

/// `message`, framed for writing.
pub fn frame(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
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
