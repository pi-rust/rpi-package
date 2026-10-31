//! ACP client filesystem authority. No source filesystem access or fallback.
use crate::protocol::{ClientRequestError, Writer};
use rpi_extcore::file_io::*;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

pub(crate) fn capabilities(params: &Value) -> Result<FileIOCapabilities, String> {
    let Some(fs) = params.pointer("/clientCapabilities/fs") else {
        return Ok(FileIOCapabilities::default());
    };
    let fs = fs
        .as_object()
        .ok_or("clientCapabilities.fs must be an object")?;
    let flag = |key: &str| match fs.get(key) {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        _ => Err(format!("clientCapabilities.fs.{key} must be a boolean")),
    };
    Ok(FileIOCapabilities {
        read_text: flag("readTextFile")?,
        write_text: flag("writeTextFile")?,
    })
}

/// Count serialized bytes without allocating another untrusted payload copy.
pub(crate) fn within_json_budget(value: &Value, limit: usize) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or(std::io::ErrorKind::InvalidData)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(limit), value).is_ok()
}

pub(crate) struct ClientFileIO {
    pub(crate) capabilities: FileIOCapabilities,
    writer: Arc<Writer>,
    session_id: String,
}
impl ClientFileIO {
    pub(crate) fn new(writer: Arc<Writer>, session_id: String) -> Self {
        Self {
            capabilities: writer.file_io_capabilities(),
            writer,
            session_id,
        }
    }

    async fn perform(&self, request: FileIORequest) -> Result<FileIOResponse, FileIOError> {
        let (method, params, read_max) = match request.operation {
            FileIOOperation::ReadText {
                absolute_path,
                range,
                max_bytes,
            } => {
                if !self.capabilities.read_text {
                    return Err(FileIOError::new(
                        FileIOErrorCode::Unsupported,
                        "client read capability is absent",
                    ));
                }
                let mut params = json!({"sessionId":self.session_id,"path":absolute_path});
                if let FileIOReadRange::Lines {
                    offset_1based,
                    limit,
                } = range
                {
                    params["line"] = json!(offset_1based);
                    params["limit"] = json!(limit);
                }
                (
                    "fs/read_text_file",
                    params,
                    Some(max_bytes.min(MAX_TEXT_BYTES)),
                )
            }
            FileIOOperation::WriteText {
                absolute_path,
                content,
            } => {
                if !self.capabilities.write_text {
                    return Err(FileIOError::new(
                        FileIOErrorCode::Unsupported,
                        "client write capability is absent",
                    ));
                }
                (
                    "fs/write_text_file",
                    json!({"sessionId":self.session_id,"path":absolute_path,"content":content}),
                    None,
                )
            }
        };
        let result = self
            .writer
            .request_checked(method, params, Some(MAX_PAYLOAD_BYTES))
            .await
            .map_err(map_error)?;
        if let Some(max) = read_max {
            let mut fields = match result {
                Value::Object(fields) => fields,
                _ => {
                    return Err(FileIOError::new(
                        FileIOErrorCode::InvalidResponse,
                        "client read result must contain content",
                    ))
                }
            };
            match fields.remove("content") {
                Some(Value::String(content)) if content.len() <= max => {
                    Ok(FileIOResponse::Text { content })
                }
                Some(Value::String(_)) => Err(FileIOError::new(
                    FileIOErrorCode::TooLarge,
                    "client read text exceeds byte budget",
                )),
                _ => Err(FileIOError::new(
                    FileIOErrorCode::InvalidResponse,
                    "client read result must contain string content",
                )),
            }
        } else if result.is_null()
            || result
                .as_object()
                .is_some_and(|fields| fields.keys().all(|key| key == "_meta"))
        {
            // Official v1 is an empty object (optional _meta); tolerate null
            // acknowledgements without treating a JSON-RPC error as success.
            Ok(FileIOResponse::Written)
        } else {
            Err(FileIOError::new(
                FileIOErrorCode::InvalidResponse,
                "invalid client write acknowledgement",
            ))
        }
    }
}
impl FileIOHandler for ClientFileIO {
    fn request(
        &self,
        request: FileIORequest,
        cancel: FileIOCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<FileIOResponse, FileIOError>> + Send + '_>> {
        Box::pin(async move {
            // Total deadline includes queued frame write. Dropping the checked
            // request retires correlation; FrameWrite closes a partial stream.
            // Cancellation does not roll back an already committed client write.
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(FileIOError::new(FileIOErrorCode::Cancelled, "client file request cancelled")),
                _ = self.writer.terminal() => Err(FileIOError::new(FileIOErrorCode::Disconnected, "ACP transport closed")),
                result = tokio::time::timeout(Duration::from_secs(45), self.perform(request)) => {
                    result.unwrap_or_else(|_| Err(FileIOError::new(FileIOErrorCode::Timeout, "client file request timed out")))
                }
            }
        })
    }
}
fn map_error(error: ClientRequestError) -> FileIOError {
    let (code, message) = match error {
        ClientRequestError::Rpc { code, message } => {
            let mapped = match code {
                -32000 => FileIOErrorCode::Denied,
                -32002 => FileIOErrorCode::NotFound,
                -32601 => FileIOErrorCode::Unsupported,
                -32800 => FileIOErrorCode::Cancelled,
                _ => FileIOErrorCode::InvalidResponse,
            };
            (mapped, format!("ACP client error {code}: {message}"))
        }
        ClientRequestError::Cancelled => (
            FileIOErrorCode::Cancelled,
            "client request cancelled".into(),
        ),
        ClientRequestError::Disconnected => {
            (FileIOErrorCode::Disconnected, "ACP transport closed".into())
        }
        ClientRequestError::TooLarge => (
            FileIOErrorCode::TooLarge,
            "client response exceeds byte budget".into(),
        ),
        ClientRequestError::Capacity => (
            FileIOErrorCode::Denied,
            "ACP pending client request capacity exceeded".into(),
        ),
        ClientRequestError::InvalidResponse => (
            FileIOErrorCode::InvalidResponse,
            "malformed client response".into(),
        ),
    };
    // Keep the complete error including our prefix within the native budget.
    let mut end = message.len().min(MAX_ERROR_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    FileIOError::new(code, &message[..end])
}

#[cfg(test)]
#[path = "file_io_tests.rs"]
mod tests;
