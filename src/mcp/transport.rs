//! Bounded newline-delimited JSON-RPC transport for stdio MCP.
//!
//! `rmcp`'s stock async reader uses an unbounded `read_until` buffer.  MCP
//! stdio is still newline-delimited JSON-RPC here, but each inbound and
//! outbound record has a fixed byte budget and an overlong inbound record
//! closes the transport before JSON parsing can allocate more memory.

use std::{
    collections::HashMap,
    io,
    io::Write as _,
    marker::PhantomData,
    sync::{Arc, Mutex as StdMutex},
};

use rmcp::{
    ErrorData,
    model::{JsonRpcMessage, RequestId},
    service::{RxJsonRpcMessage, ServiceRole, TxJsonRpcMessage},
    transport::Transport,
};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
};

/// Do not let one hostile stdin record make the daemon retain an unbounded
/// line buffer.  This is deliberately smaller than the outbound allowance:
/// a legitimate 512 KiB binary log is represented as a JSON byte array and
/// can grow several times while being serialized.
pub(crate) const MAX_MCP_INBOUND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_MCP_OUTBOUND_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_MCP_INFLIGHT_REQUESTS: usize = 16;

pub(crate) struct BoundedStdioTransport<Role: ServiceRole, R: AsyncRead, W: AsyncWrite> {
    read: BufReader<R>,
    line: Vec<u8>,
    inbound_limit: usize,
    outbound_limit: usize,
    write: Arc<Mutex<Option<W>>>,
    admission: Arc<Semaphore>,
    requests: Arc<StdMutex<HashMap<RequestId, OwnedSemaphorePermit>>>,
    _role: PhantomData<fn() -> Role>,
}

impl<Role, R, W> BoundedStdioTransport<Role, R, W>
where
    Role: ServiceRole,
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    pub(crate) fn new(read: R, write: W) -> Self {
        Self::new_with_limits(
            read,
            write,
            MAX_MCP_INBOUND_BYTES,
            MAX_MCP_OUTBOUND_BYTES,
            MAX_MCP_INFLIGHT_REQUESTS,
        )
    }

    #[cfg(test)]
    fn new_with_limit(read: R, write: W, limit: usize) -> Self {
        Self::new_with_limits(read, write, limit, limit, MAX_MCP_INFLIGHT_REQUESTS)
    }

    fn new_with_limits(
        read: R,
        write: W,
        inbound_limit: usize,
        outbound_limit: usize,
        inflight_limit: usize,
    ) -> Self {
        Self {
            read: BufReader::new(read),
            line: Vec::new(),
            inbound_limit,
            outbound_limit,
            write: Arc::new(Mutex::new(Some(write))),
            admission: Arc::new(Semaphore::new(inflight_limit)),
            requests: Arc::new(StdMutex::new(HashMap::new())),
            _role: PhantomData,
        }
    }

    /// Read exactly one newline-delimited record without ever extending the
    /// retained line allocation past `limit`.  EOF intentionally discards an
    /// unterminated trailing record, matching rmcp's stdio behavior.
    async fn read_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            let available = self.read.fill_buf().await?;
            if available.is_empty() {
                self.line.clear();
                return Ok(None);
            }
            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if self.line.len().saturating_add(consumed) > self.inbound_limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP JSON-RPC line exceeds the configured limit",
                ));
            }
            let complete = available[consumed - 1] == b'\n';
            let fragment = available[..consumed].to_vec();
            self.read.consume(consumed);
            self.line.extend_from_slice(&fragment);
            if complete {
                return Ok(Some(std::mem::take(&mut self.line)));
            }
        }
    }
}

impl<Role, R, W> Transport<Role> for BoundedStdioTransport<Role, R, W>
where
    Role: ServiceRole,
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<Role>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let write = Arc::clone(&self.write);
        let response_id = match &item {
            JsonRpcMessage::Response(response) => Some(&response.id),
            JsonRpcMessage::Error(error) => error.id.as_ref(),
            _ => None,
        };
        // Keep the permit in the send future until stdout has drained. This
        // bounds both handler tasks and independently encoded responses when a
        // client keeps writing requests but stops reading its response pipe.
        let permit = response_id.and_then(|id| {
            self.requests
                .lock()
                .expect("MCP request admission lock poisoned")
                .remove(id)
        });
        let limit = self.outbound_limit;
        async move {
            let _permit = permit;
            let encoded = encode_line(&item, limit)?;
            let mut write = write.lock().await;
            let output = write.as_mut().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "MCP transport is closed")
            })?;
            output.write_all(&encoded).await?;
            output.flush().await
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<Role>> {
        loop {
            let line = match self.read_line().await {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => return None,
            };
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<RxJsonRpcMessage<Role>>(line) {
                Ok(message) => {
                    if let JsonRpcMessage::Request(request) = &message {
                        let admitted = {
                            let mut requests = self
                                .requests
                                .lock()
                                .expect("MCP request admission lock poisoned");
                            // Reusing an in-flight id would make response-based
                            // permit release ambiguous, so fail the connection.
                            if requests.contains_key(&request.id) {
                                return None;
                            }
                            Arc::clone(&self.admission)
                                .try_acquire_owned()
                                .map(|permit| requests.insert(request.id.clone(), permit))
                                .is_ok()
                        };
                        if !admitted {
                            let response = TxJsonRpcMessage::<Role>::error(
                                ErrorData::internal_error(
                                    "MCP request limit reached; retry after an in-flight request completes",
                                    None,
                                ),
                                Some(request.id.clone()),
                            );
                            if self.send(response).await.is_err() {
                                return None;
                            }
                            continue;
                        }
                    }
                    return Some(message);
                }
                Err(error)
                    if matches!(
                        error.classify(),
                        serde_json::error::Category::Syntax | serde_json::error::Category::Eof
                    ) => {}
                Err(_) => {
                    let response = TxJsonRpcMessage::<Role>::error(
                        ErrorData::invalid_request("Invalid request", None),
                        None,
                    );
                    if self.send(response).await.is_err() {
                        return None;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.write.lock().await.take();
        Ok(())
    }
}

fn encode_line(value: &impl Serialize, limit: usize) -> io::Result<Vec<u8>> {
    let mut writer = LimitedVec::new(limit);
    serde_json::to_writer(&mut writer, value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writer.write_all(b"\n")?;
    Ok(writer.into_inner())
}

struct LimitedVec {
    bytes: Vec<u8>,
    limit: usize,
}

impl LimitedVec {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl io::Write for LimitedVec {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(buffer.len()) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP JSON-RPC output exceeds the configured limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::service::RoleServer;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn oversized_stdio_line_is_rejected_before_json_parsing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut input, output) = tokio::io::duplex(512);
            let mut transport = BoundedStdioTransport::<RoleServer, _, _>::new_with_limit(
                output,
                tokio::io::sink(),
                64,
            );
            input.write_all(&[b'x'; 65]).await.unwrap();
            input.write_all(b"\n").await.unwrap();
            assert!(transport.read_line().await.is_err());
        });
    }

    #[test]
    fn outbound_json_is_bounded_and_newline_delimited() {
        let line = encode_line(&serde_json::json!({"ok":true}), 64).unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        assert!(encode_line(&serde_json::json!({"value":"x".repeat(80)}), 64).is_err());
    }

    #[test]
    fn outbound_budget_accepts_the_largest_log_payload_after_json_expansion() {
        // A byte array is the worst normal MCP representation of a log: each
        // byte can consume four JSON bytes including its separator.
        let payload = vec![255_u8; 512 * 1024];
        let line = encode_line(
            &serde_json::json!({"bytes": payload}),
            MAX_MCP_OUTBOUND_BYTES,
        )
        .unwrap();
        assert!(line.len() <= MAX_MCP_OUTBOUND_BYTES);
    }

    #[test]
    fn request_admission_rejects_overload_and_releases_after_response_drain() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client_input, server_input) = tokio::io::duplex(4096);
            let (server_output, client_output) = tokio::io::duplex(4096);
            let mut transport = BoundedStdioTransport::<RoleServer, _, _>::new_with_limits(
                server_input,
                server_output,
                4096,
                4096,
                1,
            );
            client_input
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\
                      {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
                )
                .await
                .unwrap();
            client_input.shutdown().await.unwrap();

            let first = transport.receive().await.unwrap();
            let first_id = match first {
                JsonRpcMessage::Request(request) => request.id,
                _ => panic!("expected request"),
            };
            assert_eq!(transport.admission.available_permits(), 0);
            assert!(transport.receive().await.is_none());

            let mut reader = BufReader::new(client_output);
            let mut rejection = String::new();
            reader.read_line(&mut rejection).await.unwrap();
            assert!(rejection.contains("MCP request limit reached"));
            assert!(rejection.contains("\"id\":2"));

            transport
                .send(TxJsonRpcMessage::<RoleServer>::error(
                    ErrorData::internal_error("done", None),
                    Some(first_id),
                ))
                .await
                .unwrap();
            assert_eq!(transport.admission.available_permits(), 1);
        });
    }

    #[test]
    fn receive_skips_blank_syntax_and_crlf_but_rejects_duplicate_ids() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let input = b"\n{bad\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\r\n\
                          {\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
            let mut transport = BoundedStdioTransport::<RoleServer, _, _>::new_with_limit(
                &input[..],
                tokio::io::sink(),
                512,
            );
            assert!(matches!(
                transport.receive().await,
                Some(JsonRpcMessage::Request(_))
            ));
            assert!(transport.receive().await.is_none());
        });
    }

    #[test]
    fn invalid_request_gets_an_error_and_closed_transport_rejects_send() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let input = b"[]\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
            let (server_output, client_output) = tokio::io::duplex(1024);
            let mut transport = BoundedStdioTransport::<RoleServer, _, _>::new_with_limit(
                &input[..],
                server_output,
                1024,
            );
            assert!(matches!(
                transport.receive().await,
                Some(JsonRpcMessage::Notification(_))
            ));
            let mut reader = BufReader::new(client_output);
            let mut error = String::new();
            reader.read_line(&mut error).await.unwrap();
            assert!(error.contains("\"code\":-32600"));

            transport.close().await.unwrap();
            assert!(
                transport
                    .send(TxJsonRpcMessage::<RoleServer>::error(
                        ErrorData::internal_error("closed", None),
                        None,
                    ))
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn unterminated_eof_is_discarded_and_limited_writer_flushes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut transport = BoundedStdioTransport::<RoleServer, _, _>::new_with_limit(
                &b"unterminated"[..],
                tokio::io::sink(),
                64,
            );
            assert!(transport.read_line().await.unwrap().is_none());
        });

        let mut writer = LimitedVec::new(4);
        io::Write::write_all(&mut writer, b"four").unwrap();
        io::Write::flush(&mut writer).unwrap();
        assert!(io::Write::write_all(&mut writer, b"!").is_err());
        assert_eq!(writer.into_inner(), b"four");
    }
}
