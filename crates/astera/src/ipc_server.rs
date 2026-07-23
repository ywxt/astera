use std::{
    fs,
    future::Future,
    io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::{Context, Result};
use astera_ipc::{
    BOOTSTRAP_VERSION, CURRENT_VERSION, Error, ErrorCode, MIN_VERSION, Request, RequestDecodeError,
    RequestKind, Response, VersionedRequest, decode_payload, decode_request, encode_frame,
    parse_frame, wire,
};
use smol::{
    Executor, Timer,
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::unix::{UnixListener, UnixStream},
};
use thiserror::Error;

use crate::state::Astera;

const MAX_CLIENTS: usize = 64;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

struct PendingRequest {
    // The async socket task owns framing; the compositor thread alone executes this command.
    request: Request,
    reply: smol::channel::Sender<Response>,
}

#[derive(Debug, Error)]
pub enum IpcServerError {
    #[error("XDG_RUNTIME_DIR is required for the IPC socket")]
    MissingRuntimeDirectory,
    #[error("IPC socket {0} is already active")]
    AddressInUse(PathBuf),
    #[error("could not {operation} IPC socket {path}")]
    Socket {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Cooperative IPC server driven by the compositor event loop.
///
/// No IPC accept or per-client worker threads are created. Socket tasks are cooperatively ticked
/// by `dispatch`, keeping all compositor state access on its owning thread.
pub struct IpcServer {
    executor: Arc<Executor<'static>>,
    requests: mpsc::Receiver<PendingRequest>,
    pub path: PathBuf,
}

impl IpcServer {
    pub fn bind(display_name: &str) -> std::result::Result<Self, IpcServerError> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(IpcServerError::MissingRuntimeDirectory)?;
        let path = runtime.join(format!("{display_name}.ipc"));
        remove_stale_socket(&path)?;

        let listener = UnixListener::bind(&path).map_err(|source| IpcServerError::Socket {
            operation: "bind",
            path: path.clone(),
            source,
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            IpcServerError::Socket {
                operation: "set permissions on",
                path: path.clone(),
                source,
            }
        })?;
        let executor = Arc::new(Executor::new());
        let (sender, requests) = mpsc::sync_channel(MAX_CLIENTS);
        let accept_executor = executor.clone();
        let log_path = path.clone();
        executor
            .spawn(async move {
                let clients = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                loop {
                    let (stream, _) = match listener.accept().await {
                        Ok(client) => client,
                        Err(error) => {
                            tracing::warn!(%error, path = %log_path.display(), "IPC accept failed");
                            break;
                        }
                    };
                    if clients.load(std::sync::atomic::Ordering::Acquire) >= MAX_CLIENTS {
                        tracing::warn!(path = %log_path.display(), "IPC client limit reached");
                        continue;
                    }
                    clients.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    let sender = sender.clone();
                    let clients = clients.clone();
                    let path = log_path.clone();
                    accept_executor
                        .spawn(async move {
                            if let Err(error) = handle_client(stream, sender).await {
                                tracing::warn!(%error, path = %path.display(), "IPC request failed");
                            }
                            clients.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                        })
                        .detach();
                }
            })
            .detach();
        tracing::info!(path = %path.display(), "IPC server is ready");
        Ok(Self {
            executor,
            requests,
            path,
        })
    }

    pub fn dispatch(&self, state: &mut Astera) {
        // Tick before draining to accept/read requests, and after replying so completed writes do
        // not wait for the next render iteration.
        self.drain_tasks();
        while let Ok(pending) = self.requests.try_recv() {
            let sequence = state.public_sequence();
            let response = match pending.request.kind {
                RequestKind::Command(command) => state.execute_command_at(command, sequence),
                RequestKind::EventStream => Response::Error(Error {
                    code: ErrorCode::InvalidRequest,
                    message: "event streams are not implemented".into(),
                    sequence,
                }),
            };
            let _ = pending.reply.try_send(response);
        }
        self.drain_tasks();
    }

    fn drain_tasks(&self) {
        while self.executor.try_tick() {}
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn remove_stale_socket(path: &PathBuf) -> std::result::Result<(), IpcServerError> {
    if !path.exists() {
        return Ok(());
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(IpcServerError::AddressInUse(path.clone())),
        Err(_) => fs::remove_file(path).map_err(|source| IpcServerError::Socket {
            operation: "remove stale",
            path: path.clone(),
            source,
        }),
    }
}

async fn handle_client(
    mut stream: UnixStream,
    sender: mpsc::SyncSender<PendingRequest>,
) -> Result<()> {
    let payload = {
        let mut reader = BufReader::new(&mut stream);
        with_timeout(IO_TIMEOUT, read_frame_line(&mut reader))
            .await
            .context("timed out while reading IPC request")??
    };
    let frame = parse_frame(&payload).context("invalid IPC frame")?;
    if frame.version == BOOTSTRAP_VERSION {
        let _: wire::v0::Request = decode_payload(frame).context("invalid bootstrap request")?;
        return write_bootstrap_response(
            &mut stream,
            wire::v0::Response::Versions {
                minimum: MIN_VERSION,
                current: CURRENT_VERSION,
            },
        )
        .await;
    }
    let request = match decode_request(&payload) {
        Ok(VersionedRequest::V1(request)) => request,
        Err(RequestDecodeError::UnsupportedVersion {
            requested,
            minimum,
            current,
        }) => {
            return write_bootstrap_response(
                &mut stream,
                wire::v0::Response::UnsupportedVersion {
                    requested,
                    minimum,
                    current,
                },
            )
            .await;
        }
        Err(error) => {
            let response = Response::Error(Error {
                code: ErrorCode::InvalidRequest,
                message: error.to_string(),
                sequence: 0,
            });
            return write_response(&mut stream, CURRENT_VERSION, response).await;
        }
    };
    let (reply, response) = smol::channel::bounded(1);
    sender
        .try_send(PendingRequest { request, reply })
        .context("IPC command queue is full")?;
    let response = with_timeout(Duration::from_secs(5), response.recv())
        .await
        .context("timed out waiting for compositor response")?
        .context("compositor dropped IPC response")?;
    write_response(&mut stream, CURRENT_VERSION, response).await
}

async fn read_frame_line<R: smol::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<String> {
    let mut bytes = Vec::with_capacity(1024);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            anyhow::bail!("IPC client closed before a complete frame");
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |newline| newline + 1);
        if bytes.len() + consumed > MAX_REQUEST_BYTES as usize {
            anyhow::bail!("IPC request exceeds {MAX_REQUEST_BYTES} bytes");
        }
        let complete = available[consumed - 1] == b'\n';
        bytes.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if complete {
            return String::from_utf8(bytes).context("IPC request is not UTF-8");
        }
    }
}

async fn write_bootstrap_response(
    stream: &mut UnixStream,
    response: wire::v0::Response,
) -> Result<()> {
    let payload = encode_frame(BOOTSTRAP_VERSION, &response)
        .context("could not serialize IPC bootstrap response")?;
    with_timeout(IO_TIMEOUT, stream.write_all(payload.as_bytes()))
        .await
        .context("timed out while writing IPC bootstrap response")??;
    Ok(())
}

async fn write_response(stream: &mut UnixStream, version: u16, response: Response) -> Result<()> {
    let payload = encode_frame(version, &response).context("could not serialize IPC response")?;
    with_timeout(IO_TIMEOUT, stream.write_all(payload.as_bytes()))
        .await
        .context("timed out while writing IPC response")??;
    Ok(())
}

async fn with_timeout<T>(duration: Duration, future: impl Future<Output = T>) -> io::Result<T> {
    smol::future::race(async move { Ok(future.await) }, async move {
        Timer::after(duration).await;
        Err(io::Error::new(io::ErrorKind::TimedOut, "IPC timed out"))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_reader_finishes_without_waiting_for_eof() {
        smol::block_on(async {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            client
                .write_all(b"1 (kind:EventStream)\n1 (kind:EventStream)\n")
                .await
                .unwrap();
            // The client intentionally remains open: newline, rather than EOF, terminates a frame.
            let mut reader = BufReader::new(&mut server);
            assert_eq!(
                read_frame_line(&mut reader).await.unwrap(),
                "1 (kind:EventStream)\n"
            );
            assert_eq!(
                read_frame_line(&mut reader).await.unwrap(),
                "1 (kind:EventStream)\n"
            );
        });
    }

    #[test]
    fn line_reader_enforces_the_exact_byte_limit() {
        smol::block_on(async {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let mut oversized = vec![b'x'; MAX_REQUEST_BYTES as usize];
            oversized.push(b'\n');
            let write = async { client.write_all(&oversized).await.unwrap() };
            let read = async {
                let mut reader = BufReader::new(&mut server);
                read_frame_line(&mut reader).await
            };
            let (_, result) = smol::future::zip(write, read).await;
            assert!(result.unwrap_err().to_string().contains("exceeds"));
        });
    }
}
