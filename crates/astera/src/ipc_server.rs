use std::{
    fs,
    future::Future,
    io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsFd,
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use astera_ipc::{
    BOOTSTRAP_VERSION, CURRENT_VERSION, Error, ErrorCode, MIN_VERSION, Request, RequestKind,
    Response, Success, decode_payload, encode_frame, parse_frame, wire,
    wire::v1::{DesktopSnapshot, EventEnvelope},
};
use smol::{
    Executor, Timer,
    channel::{Receiver, Sender},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::unix::{UnixListener, UnixStream},
};
use thiserror::Error;

use crate::state::Astera;

const MAX_COMMAND_CONNECTIONS: usize = 128;
const MAX_SUBSCRIBERS: usize = 64;
const SUBSCRIBER_QUEUE: usize = 256;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

struct PendingRequest {
    request: Request,
    reply: Sender<MainReply>,
}

struct PendingCommand {
    response: Response,
    reply: Sender<MainReply>,
}

struct Subscriber {
    events: Sender<Arc<EventEnvelope>>,
    cancel: Sender<()>,
}

enum MainReply {
    Response(Response),
    EventStream {
        sequence: u64,
        snapshot: DesktopSnapshot,
        events: Receiver<Arc<EventEnvelope>>,
        cancel: Receiver<()>,
    },
}

#[derive(Debug, Error)]
pub enum IpcServerError {
    #[error("XDG_RUNTIME_DIR is required for the IPC socket")]
    MissingRuntimeDirectory,
    #[error("IPC socket {0} is already active")]
    AddressInUse(PathBuf),
    #[error("refusing unsafe IPC path {0}")]
    UnsafePath(PathBuf),
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
/// Socket tasks only perform framing and I/O. Commands, stream registration, snapshots and
/// sequence assignment remain on the compositor thread, which provides the atomic snapshot/event
/// boundary without a mutex around compositor state.
pub struct IpcServer {
    executor: Arc<Executor<'static>>,
    requests: mpsc::Receiver<PendingRequest>,
    commands: Vec<PendingCommand>,
    stream_requests: Vec<Sender<MainReply>>,
    subscribers: Vec<Subscriber>,
    published_sequence: Arc<AtomicU64>,
    socket_identity: Option<SocketIdentity>,
    pub path: PathBuf,
}

impl IpcServer {
    pub fn bind(display_name: &str) -> std::result::Result<Self, IpcServerError> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(IpcServerError::MissingRuntimeDirectory)?;
        let directory = runtime.join("astera");
        ensure_private_directory(&directory)?;
        let path = directory.join(format!("{display_name}.ipc"));
        Self::bind_path(path)
    }

    fn bind_path(path: PathBuf) -> std::result::Result<Self, IpcServerError> {
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
        let socket_identity = Some(socket_identity(&path)?);
        let executor = Arc::new(Executor::new());
        let published_sequence = Arc::new(AtomicU64::new(0));
        let (sender, requests) = mpsc::sync_channel(MAX_COMMAND_CONNECTIONS);
        let accept_executor = executor.clone();
        let accept_sequence = published_sequence.clone();
        let log_path = path.clone();
        executor
            .spawn(async move {
                let command_connections = Arc::new(AtomicUsize::new(0));
                loop {
                    let (stream, _) = match listener.accept().await {
                        Ok(client) => client,
                        Err(error) => {
                            tracing::warn!(%error, path = %log_path.display(), "IPC accept failed");
                            break;
                        }
                    };
                    if !same_uid(&stream) {
                        tracing::warn!(path = %log_path.display(), "rejected IPC peer with different UID");
                        continue;
                    }
                    if command_connections
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                            (current < MAX_COMMAND_CONNECTIONS).then_some(current + 1)
                        })
                        .is_err()
                    {
                        tracing::warn!(path = %log_path.display(), "IPC command connection limit reached");
                        // Waiting for an over-limit peer to reveal its version would create an
                        // unbounded collection of idle rejection tasks.
                        drop(stream);
                        continue;
                    }
                    let sender = sender.clone();
                    let published_sequence = accept_sequence.clone();
                    let path = log_path.clone();
                    let slot = CommandSlot::new(command_connections.clone());
                    accept_executor
                        .spawn(async move {
                            if let Err(error) =
                                handle_client(stream, sender, slot, published_sequence).await
                            {
                                tracing::warn!(%error, path = %path.display(), "IPC connection failed");
                            }
                        })
                        .detach();
                }
            })
            .detach();
        tracing::info!(path = %path.display(), "IPC server is ready");
        Ok(Self {
            executor,
            requests,
            commands: Vec::new(),
            stream_requests: Vec::new(),
            subscribers: Vec::new(),
            published_sequence,
            socket_identity,
            path,
        })
    }

    /// Execute all requests currently delivered by socket tasks. Responses remain pending until
    /// `finish_tick`, so every command handled in one compositor tick observes the same final
    /// sequence watermark.
    pub fn dispatch(&mut self, state: &mut Astera) {
        self.drain_tasks();
        while let Ok(pending) = self.requests.try_recv() {
            match pending.request.kind {
                RequestKind::Command(command) => {
                    self.commands.push(PendingCommand {
                        response: state.execute_command_at(command, 0),
                        reply: pending.reply,
                    });
                }
                RequestKind::EventStream => {
                    self.stream_requests.push(pending.reply);
                }
            }
        }
        self.drain_tasks();
    }

    /// Publish the tick, route its events, complete command responses, then atomically register new
    /// streams from the resulting snapshot. New subscribers therefore start at N and can only
    /// receive events N+1 onward.
    pub fn finish_tick(&mut self, state: &mut Astera) {
        let events = state
            .publish_public_state()
            .iter()
            .cloned()
            .map(Arc::new)
            .collect::<Vec<_>>();
        if state.take_public_sequence_overflow() {
            tracing::warn!("IPC event sequence exhausted; disconnecting all subscribers");
            for subscriber in self.subscribers.drain(..) {
                disconnect_subscriber(&subscriber);
            }
        }
        if !events.is_empty() {
            self.route_events(&events);
        }

        let sequence = state.public_sequence();
        self.published_sequence.store(sequence, Ordering::Release);
        let needs_snapshot =
            self.commands.iter().any(|pending| {
                matches!(pending.response, Response::Success(Success::State { .. }))
            }) || !self.stream_requests.is_empty();
        let snapshot = needs_snapshot.then(|| state.public_snapshot());
        for pending in self.commands.drain(..) {
            let response = finalize_response(pending.response, sequence, snapshot.as_ref());
            let _ = pending.reply.try_send(MainReply::Response(response));
        }

        self.subscribers
            .retain(|subscriber| !subscriber.events.is_closed());
        for reply in self.stream_requests.drain(..) {
            if self.subscribers.len() >= MAX_SUBSCRIBERS {
                let _ = reply.try_send(MainReply::Response(Response::Error(Error {
                    code: ErrorCode::Conflict,
                    message: "event stream subscriber limit reached".into(),
                    sequence,
                })));
                continue;
            }
            let (sender, events) = smol::channel::bounded(SUBSCRIBER_QUEUE);
            let (cancel, cancellation) = smol::channel::bounded(1);
            self.subscribers.push(Subscriber {
                events: sender,
                cancel,
            });
            let _ = reply.try_send(MainReply::EventStream {
                sequence,
                snapshot: snapshot
                    .as_ref()
                    .expect("stream registration requested a snapshot")
                    .clone(),
                events,
                cancel: cancellation,
            });
        }
        self.drain_tasks();
    }

    fn route_events(&mut self, events: &[Arc<EventEnvelope>]) {
        self.subscribers.retain(|subscriber| {
            for event in events {
                if subscriber.events.try_send(event.clone()).is_err() {
                    disconnect_subscriber(subscriber);
                    tracing::warn!(
                        sequence = event.sequence,
                        capacity = SUBSCRIBER_QUEUE,
                        "disconnecting lagged IPC event subscriber"
                    );
                    return false;
                }
            }
            true
        });
    }

    fn drain_tasks(&self) {
        while self.executor.try_tick() {}
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Some(identity) = self.socket_identity {
            let _ = remove_matching_socket(&self.path, identity);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> std::result::Result<SocketIdentity, IpcServerError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| IpcServerError::Socket {
        operation: "inspect identity of",
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(IpcServerError::UnsafePath(path.to_owned()));
    }
    Ok(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn remove_matching_socket(
    path: &Path,
    expected: SocketIdentity,
) -> std::result::Result<(), IpcServerError> {
    let actual = socket_identity(path)?;
    if actual != expected {
        return Err(IpcServerError::UnsafePath(path.to_owned()));
    }
    fs::remove_file(path).map_err(|source| IpcServerError::Socket {
        operation: "remove",
        path: path.to_owned(),
        source,
    })
}

fn finalize_response(
    response: Response,
    sequence: u64,
    snapshot: Option<&DesktopSnapshot>,
) -> Response {
    match response {
        Response::Success(Success::State { .. }) => Response::Success(Success::State {
            sequence,
            snapshot: snapshot
                .expect("state response requested a snapshot")
                .clone(),
        }),
        Response::Success(Success::Handled { .. }) => {
            Response::Success(Success::Handled { sequence })
        }
        Response::Success(Success::EventStream { .. }) => {
            unreachable!("event stream handshakes are constructed by the server")
        }
        Response::Error(mut error) => {
            error.sequence = sequence;
            Response::Error(error)
        }
    }
}

fn ensure_private_directory(path: &Path) -> std::result::Result<(), IpcServerError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(IpcServerError::UnsafePath(path.to_owned()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|source| IpcServerError::Socket {
                operation: "create parent directory for",
                path: path.to_owned(),
                source,
            })?;
        }
        Err(source) => {
            return Err(IpcServerError::Socket {
                operation: "inspect parent directory for",
                path: path.to_owned(),
                source,
            });
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        IpcServerError::Socket {
            operation: "set permissions on parent directory for",
            path: path.to_owned(),
            source,
        }
    })
}

fn remove_stale_socket(path: &Path) -> std::result::Result<(), IpcServerError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(IpcServerError::Socket {
                operation: "inspect",
                path: path.to_owned(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(IpcServerError::UnsafePath(path.to_owned()));
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(IpcServerError::AddressInUse(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => remove_matching_socket(
            path,
            SocketIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        ),
        Err(source) => Err(IpcServerError::Socket {
            operation: "probe existing",
            path: path.to_owned(),
            source,
        }),
    }
}

fn disconnect_subscriber(subscriber: &Subscriber) {
    let _ = subscriber.cancel.try_send(());
    subscriber.cancel.close();
    subscriber.events.close();
}

fn same_uid(stream: &UnixStream) -> bool {
    rustix::net::sockopt::socket_peercred(stream.as_fd())
        .is_ok_and(|credentials| credentials.uid == rustix::process::getuid())
}

struct CommandSlot {
    connections: Arc<AtomicUsize>,
    held: bool,
}

impl CommandSlot {
    fn new(connections: Arc<AtomicUsize>) -> Self {
        Self {
            connections,
            held: true,
        }
    }

    fn release(&mut self) {
        if self.held {
            self.connections.fetch_sub(1, Ordering::AcqRel);
            self.held = false;
        }
    }
}

impl Drop for CommandSlot {
    fn drop(&mut self) {
        self.release();
    }
}

async fn handle_client(
    stream: UnixStream,
    sender: mpsc::SyncSender<PendingRequest>,
    mut command_slot: CommandSlot,
    published_sequence: Arc<AtomicU64>,
) -> Result<()> {
    let mut writer = stream.clone();
    let mut reader = BufReader::new(stream);
    let mut version = None;
    loop {
        let payload = match read_frame_line(&mut reader).await {
            Ok(payload) => payload,
            Err(error) => {
                if version.is_some()
                    && error.downcast_ref::<io::Error>().is_some_and(|error| {
                        error.kind() == io::ErrorKind::UnexpectedEof
                            && error.to_string() == "IPC client closed between frames"
                    })
                {
                    return Ok(());
                }
                if let Some(version) = version {
                    let response = invalid_request(
                        error.to_string(),
                        published_sequence.load(Ordering::Acquire),
                    );
                    let _ = write_response(&mut writer, version, &response).await;
                } else {
                    let _ = write_bootstrap_response(
                        &mut writer,
                        wire::v0::Response::InvalidFrame {
                            message: error.to_string(),
                        },
                    )
                    .await;
                }
                return Err(error);
            }
        };
        let frame = match parse_frame(&payload) {
            Ok(frame) => frame,
            Err(error) => {
                if let Some(version) = version {
                    let response = invalid_request(
                        error.to_string(),
                        published_sequence.load(Ordering::Acquire),
                    );
                    write_response(&mut writer, version, &response).await?;
                } else {
                    write_bootstrap_response(
                        &mut writer,
                        wire::v0::Response::InvalidFrame {
                            message: error.to_string(),
                        },
                    )
                    .await?;
                }
                return Ok(());
            }
        };
        if version.is_none() && frame.version == BOOTSTRAP_VERSION {
            if let Err(error) = decode_payload::<wire::v0::Request>(frame) {
                write_bootstrap_response(
                    &mut writer,
                    wire::v0::Response::InvalidRequest {
                        message: error.to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
            return write_bootstrap_response(
                &mut writer,
                wire::v0::Response::Versions {
                    minimum: MIN_VERSION,
                    current: CURRENT_VERSION,
                },
            )
            .await;
        }
        if frame.version != CURRENT_VERSION {
            if let Some(expected) = version {
                let response = invalid_request(
                    format!(
                        "IPC frame version {} does not match locked version {expected}",
                        frame.version
                    ),
                    published_sequence.load(Ordering::Acquire),
                );
                write_response(&mut writer, expected, &response).await?;
            } else {
                write_bootstrap_response(
                    &mut writer,
                    wire::v0::Response::UnsupportedVersion {
                        requested: frame.version,
                        minimum: MIN_VERSION,
                        current: CURRENT_VERSION,
                    },
                )
                .await?;
            }
            return Ok(());
        }
        if version
            .replace(frame.version)
            .is_some_and(|locked| locked != frame.version)
        {
            unreachable!("version mismatch was handled above");
        }
        let request: Request = match decode_payload(frame) {
            Ok(request) => request,
            Err(error) => {
                let response = invalid_request(
                    error.to_string(),
                    published_sequence.load(Ordering::Acquire),
                );
                write_response(&mut writer, frame.version, &response).await?;
                return Ok(());
            }
        };
        let upgrading = matches!(request.kind, RequestKind::EventStream);
        let (reply, response) = smol::channel::bounded(1);
        if sender.try_send(PendingRequest { request, reply }).is_err() {
            let response = Response::Error(Error {
                code: ErrorCode::Conflict,
                message: "compositor command queue is full".into(),
                sequence: published_sequence.load(Ordering::Acquire),
            });
            write_response(&mut writer, frame.version, &response).await?;
            return Ok(());
        }
        match response
            .recv()
            .await
            .context("compositor dropped IPC response")?
        {
            MainReply::Response(response) => {
                write_response(&mut writer, frame.version, &response).await?;
                if upgrading {
                    return Ok(());
                }
            }
            MainReply::EventStream {
                sequence,
                snapshot,
                events,
                cancel,
            } => {
                let response = Response::Success(Success::EventStream { sequence, snapshot });
                write_stream_handshake(&mut writer, frame.version, &response, cancel).await?;
                command_slot.release();
                return stream_events(&mut writer, frame.version, events).await;
            }
        }
    }
}

async fn write_stream_handshake(
    writer: &mut UnixStream,
    version: u16,
    response: &Response,
    cancel: Receiver<()>,
) -> Result<()> {
    let write = write_response(writer, version, response);
    let cancelled = async move {
        let _ = cancel.recv().await;
        anyhow::bail!("event stream cancelled while writing initial snapshot")
    };
    smol::future::race(write, cancelled).await
}

async fn stream_events(
    writer: &mut UnixStream,
    version: u16,
    events: Receiver<Arc<EventEnvelope>>,
) -> Result<()> {
    while let Ok(event) = events.recv().await {
        if events.is_closed() {
            break;
        }
        let payload =
            encode_frame(version, event.as_ref()).context("could not serialize IPC event")?;
        with_timeout(IO_TIMEOUT, writer.write_all(payload.as_bytes()))
            .await
            .context("timed out while writing IPC event")??;
    }
    Ok(())
}

fn invalid_request(message: String, sequence: u64) -> Response {
    Response::Error(Error {
        code: ErrorCode::InvalidRequest,
        message,
        sequence,
    })
}

async fn read_frame_line<R: smol::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                if bytes.is_empty() {
                    "IPC client closed between frames"
                } else {
                    "IPC client closed before a complete frame"
                },
            )
            .into());
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |newline| newline + 1);
        let complete = available[consumed - 1] == b'\n';
        if !oversized && bytes.len() + consumed <= MAX_REQUEST_BYTES {
            bytes.extend_from_slice(&available[..consumed]);
        } else {
            oversized = true;
        }
        reader.consume(consumed);
        if complete {
            if oversized {
                anyhow::bail!("IPC request exceeds {MAX_REQUEST_BYTES} bytes");
            }
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

async fn write_response(stream: &mut UnixStream, version: u16, response: &Response) -> Result<()> {
    let payload = encode_frame(version, response).context("could not serialize IPC response")?;
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
    use astera_ipc::{Command, VersionedRequest, decode_frame, decode_request};
    use smithay::reexports::wayland_server::Display;
    use smol::io::AsyncReadExt;
    use std::{
        io::{BufRead, BufReader as SyncBufReader, Write},
        os::unix::fs::{PermissionsExt, symlink},
        sync::mpsc as sync_mpsc,
        thread,
        time::{Instant, SystemTime},
    };

    #[test]
    fn line_reader_finishes_without_waiting_for_eof() {
        smol::block_on(async {
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .write_all(b"1 (kind:EventStream)\n1 (kind:EventStream)\n")
                .await
                .unwrap();
            let mut reader = BufReader::new(server);
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
    fn line_reader_discards_an_oversized_line_and_keeps_the_next_frame() {
        smol::block_on(async {
            let (mut client, server) = UnixStream::pair().unwrap();
            let mut oversized = vec![b'x'; MAX_REQUEST_BYTES];
            oversized.extend_from_slice(b"\n1 (kind:EventStream)\n");
            let write = async { client.write_all(&oversized).await.unwrap() };
            let read = async {
                let mut reader = BufReader::new(server);
                assert!(read_frame_line(&mut reader).await.is_err());
                read_frame_line(&mut reader).await.unwrap()
            };
            let (_, next) = smol::future::zip(write, read).await;
            assert_eq!(next, "1 (kind:EventStream)\n");
        });
    }

    #[test]
    fn first_oversized_frame_receives_a_bootstrap_error() {
        smol::block_on(async {
            let (mut client, server) = UnixStream::pair().unwrap();
            let (sender, _requests) = mpsc::sync_channel(1);
            let connections = Arc::new(AtomicUsize::new(1));
            let published_sequence = Arc::new(AtomicU64::new(0));
            let request = async {
                let mut oversized = vec![b'x'; MAX_REQUEST_BYTES + 1];
                oversized.push(b'\n');
                client.write_all(&oversized).await.unwrap();
                let mut response = String::new();
                BufReader::new(client)
                    .read_line(&mut response)
                    .await
                    .unwrap();
                response
            };
            let server = handle_client(
                server,
                sender,
                CommandSlot::new(connections),
                published_sequence,
            );
            let (response, result) = smol::future::zip(request, server).await;
            assert!(result.is_err());
            assert!(matches!(
                decode_frame::<wire::v0::Response>(&response, BOOTSTRAP_VERSION).unwrap(),
                wire::v0::Response::InvalidFrame { .. }
            ));
        });
    }

    #[test]
    fn malformed_bootstrap_payload_receives_a_structured_error() {
        smol::block_on(async {
            let (mut client, server) = UnixStream::pair().unwrap();
            let (sender, _requests) = mpsc::sync_channel(1);
            let request = async {
                client
                    .write_all(b"0 DefinitelyNotVersions\n")
                    .await
                    .unwrap();
                let mut response = String::new();
                BufReader::new(client)
                    .read_line(&mut response)
                    .await
                    .unwrap();
                response
            };
            let server = handle_client(
                server,
                sender,
                CommandSlot::new(Arc::new(AtomicUsize::new(1))),
                Arc::new(AtomicU64::new(0)),
            );
            let (response, result) = smol::future::zip(request, server).await;
            result.unwrap();
            assert!(matches!(
                decode_frame::<wire::v0::Response>(&response, BOOTSTRAP_VERSION).unwrap(),
                wire::v0::Response::InvalidRequest { .. }
            ));
        });
    }

    #[test]
    fn cancellation_interrupts_a_blocked_snapshot_write() {
        smol::block_on(async {
            let (_client, mut server) = UnixStream::pair().unwrap();
            let (cancel, cancellation) = smol::channel::bounded(1);
            cancel.send(()).await.unwrap();
            let mut snapshot = DesktopSnapshot::default();
            snapshot.config.error = Some("x".repeat(8 * 1024 * 1024));
            let response = Response::Success(Success::EventStream {
                sequence: 0,
                snapshot,
            });
            let error =
                write_stream_handshake(&mut server, CURRENT_VERSION, &response, cancellation)
                    .await
                    .unwrap_err();
            assert!(error.to_string().contains("cancelled"));
        });
    }

    #[test]
    fn socket_write_timeout_interrupts_a_slow_reader() {
        smol::block_on(async {
            let (_client, mut server) = UnixStream::pair().unwrap();
            let payload = vec![b'x'; 8 * 1024 * 1024];
            let error = with_timeout(Duration::from_millis(10), server.write_all(&payload))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        });
    }

    #[test]
    fn closed_subscriber_does_not_drain_buffered_events() {
        smol::block_on(async {
            let (sender, receiver) = smol::channel::bounded(2);
            sender
                .send(Arc::new(EventEnvelope {
                    sequence: 1,
                    event: wire::v1::Event::Unsupported { name: "x".into() },
                }))
                .await
                .unwrap();
            sender.close();
            let (mut client, mut server) = UnixStream::pair().unwrap();
            stream_events(&mut server, CURRENT_VERSION, receiver)
                .await
                .unwrap();
            drop(server);
            let mut bytes = Vec::new();
            client.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
        });
    }

    fn temporary_socket() -> (PathBuf, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "astera-ipc-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let socket = directory.join("test.ipc");
        (directory, socket)
    }

    #[test]
    fn command_errors_multi_request_connections_and_stream_boundary_are_consistent() {
        let (directory, socket) = temporary_socket();
        let mut server = IpcServer::bind_path(socket.clone()).unwrap();
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        let (handshake_tx, handshake_rx) = sync_mpsc::sync_channel(1);
        let (result_tx, result_rx) = sync_mpsc::sync_channel(1);
        let client = thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
            let invalid_focus = encode_frame(
                CURRENT_VERSION,
                &Request {
                    kind: RequestKind::Command(Command::FocusWindow(
                        astera_ipc::wire::v1::WindowId(u64::MAX),
                    )),
                },
            )
            .unwrap();
            stream.write_all(invalid_focus.as_bytes()).unwrap();
            let mut reader = SyncBufReader::new(stream.try_clone().unwrap());
            let mut error = String::new();
            reader.read_line(&mut error).unwrap();
            let get_state = encode_frame(
                CURRENT_VERSION,
                &Request {
                    kind: RequestKind::Command(Command::GetState),
                },
            )
            .unwrap();
            stream.write_all(get_state.as_bytes()).unwrap();
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            stream.write_all(get_state.as_bytes()).unwrap();
            let mut second = String::new();
            reader.read_line(&mut second).unwrap();
            stream
                .write_all(
                    encode_frame(
                        CURRENT_VERSION,
                        &Request {
                            kind: RequestKind::EventStream,
                        },
                    )
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            let mut handshake = String::new();
            reader.read_line(&mut handshake).unwrap();
            let Response::Success(Success::EventStream { sequence, .. }) =
                decode_frame(&handshake, CURRENT_VERSION).unwrap()
            else {
                panic!("expected event stream handshake")
            };
            handshake_tx.send(sequence).unwrap();
            let mut event = String::new();
            reader.read_line(&mut event).unwrap();
            let event: EventEnvelope = decode_frame(&event, CURRENT_VERSION).unwrap();
            result_tx
                .send((error, first, second, sequence, event))
                .unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut changed = false;
        while Instant::now() < deadline {
            server.dispatch(&mut state);
            if !changed && let Ok(sequence) = handshake_rx.try_recv() {
                let workspace = state.public_snapshot().outputs[0].active_workspace;
                state.execute_command(Command::PanCamera {
                    workspace,
                    dx: 10,
                    dy: 0,
                });
                changed = true;
                assert_eq!(sequence, state.public_sequence());
            }
            server.finish_tick(&mut state);
            if let Ok((error, first, second, sequence, event)) = result_rx.try_recv() {
                let Response::Error(error) =
                    decode_frame::<Response>(&error, CURRENT_VERSION).unwrap()
                else {
                    panic!("expected command error")
                };
                assert_eq!(error.code, ErrorCode::NotFound);
                let Response::Success(Success::State {
                    sequence: first_sequence,
                    ..
                }) = decode_frame::<Response>(&first, CURRENT_VERSION).unwrap()
                else {
                    panic!("expected first state response")
                };
                let Response::Success(Success::State {
                    sequence: second_sequence,
                    ..
                }) = decode_frame::<Response>(&second, CURRENT_VERSION).unwrap()
                else {
                    panic!("expected second state response")
                };
                assert_eq!(error.sequence, first_sequence);
                assert_eq!(first_sequence, second_sequence);
                assert_eq!(second_sequence, sequence);
                assert_eq!(event.sequence, sequence + 1);
                assert!(matches!(event.event, wire::v1::Event::CameraChanged { .. }));
                client.join().unwrap();
                drop(server);
                fs::remove_dir(directory).unwrap();
                return;
            }
            thread::yield_now();
        }
        panic!("timed out waiting for socket-level event stream exchange");
    }

    #[test]
    fn queue_overflow_closes_the_subscriber() {
        let mut server = server_without_listener();
        let (subscriber, receiver) = smol::channel::bounded(SUBSCRIBER_QUEUE);
        let (cancel, cancellation) = smol::channel::bounded(1);
        server.subscribers.push(Subscriber {
            events: subscriber,
            cancel,
        });
        let events = (1..=SUBSCRIBER_QUEUE + 1)
            .map(|sequence| {
                Arc::new(EventEnvelope {
                    sequence: sequence as u64,
                    event: wire::v1::Event::Unsupported {
                        name: "load".into(),
                    },
                })
            })
            .collect::<Vec<_>>();
        server.route_events(&events);
        assert!(server.subscribers.is_empty());
        assert!(receiver.is_closed());
        assert_eq!(cancellation.try_recv(), Ok(()));
    }

    #[test]
    fn connection_version_is_locked_after_the_first_request() {
        let (directory, socket) = temporary_socket();
        let mut server = IpcServer::bind_path(socket.clone()).unwrap();
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        let (result_tx, result_rx) = sync_mpsc::sync_channel(1);
        let client = thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
            stream
                .write_all(
                    encode_frame(
                        CURRENT_VERSION,
                        &Request {
                            kind: RequestKind::Command(Command::GetState),
                        },
                    )
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            let mut reader = SyncBufReader::new(stream.try_clone().unwrap());
            let mut state = String::new();
            reader.read_line(&mut state).unwrap();
            stream
                .write_all(
                    encode_frame(BOOTSTRAP_VERSION, &wire::v0::Request::Versions)
                        .unwrap()
                        .as_bytes(),
                )
                .unwrap();
            let mut mismatch = String::new();
            reader.read_line(&mut mismatch).unwrap();
            result_tx.send((state, mismatch)).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            server.dispatch(&mut state);
            server.finish_tick(&mut state);
            if let Ok((state_response, mismatch)) = result_rx.try_recv() {
                assert!(matches!(
                    decode_frame::<Response>(&state_response, CURRENT_VERSION).unwrap(),
                    Response::Success(Success::State { .. })
                ));
                let Response::Error(error) =
                    decode_frame::<Response>(&mismatch, CURRENT_VERSION).unwrap()
                else {
                    panic!("expected version mismatch error")
                };
                assert_eq!(error.code, ErrorCode::InvalidRequest);
                assert!(error.message.contains("locked version 1"));
                assert_eq!(error.sequence, state.public_sequence());
                client.join().unwrap();
                drop(server);
                fs::remove_dir(directory).unwrap();
                return;
            }
            thread::yield_now();
        }
        panic!("timed out waiting for version mismatch response");
    }

    fn server_without_listener() -> IpcServer {
        let (_requests_tx, requests) = mpsc::sync_channel(1);
        IpcServer {
            executor: Arc::new(Executor::new()),
            requests,
            commands: Vec::new(),
            stream_requests: Vec::new(),
            subscribers: Vec::new(),
            published_sequence: Arc::new(AtomicU64::new(0)),
            socket_identity: None,
            path: PathBuf::new(),
        }
    }

    #[test]
    fn subscriber_limit_returns_a_structured_error() {
        let mut server = server_without_listener();
        let mut replies = Vec::new();
        for _ in 0..=MAX_SUBSCRIBERS {
            let (reply, response) = smol::channel::bounded(1);
            server.stream_requests.push(reply);
            replies.push(response);
        }
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        server.finish_tick(&mut state);
        assert_eq!(server.subscribers.len(), MAX_SUBSCRIBERS);
        assert!(matches!(
            replies.last().unwrap().try_recv().unwrap(),
            MainReply::Response(Response::Error(Error {
                code: ErrorCode::Conflict,
                ..
            }))
        ));
    }

    #[test]
    fn stale_cleanup_refuses_regular_files_and_symlinks() {
        let (directory, path) = temporary_socket();
        fs::write(&path, b"keep").unwrap();
        assert!(matches!(
            remove_stale_socket(&path),
            Err(IpcServerError::UnsafePath(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(&path).unwrap();
        let target = directory.join("target");
        fs::write(&target, b"keep").unwrap();
        symlink(&target, &path).unwrap();
        assert!(matches!(
            remove_stale_socket(&path),
            Err(IpcServerError::UnsafePath(_))
        ));
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        fs::remove_file(path).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn stale_cleanup_only_removes_an_unlistened_socket_node() {
        let (directory, path) = temporary_socket();
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(matches!(
            remove_stale_socket(&path),
            Err(IpcServerError::AddressInUse(_))
        ));
        drop(listener);
        remove_stale_socket(&path).unwrap();
        assert!(!path.exists());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn drop_does_not_remove_a_replacement_socket() {
        let (directory, path) = temporary_socket();
        let server = IpcServer::bind_path(path.clone()).unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(server);
        assert!(path.exists());
        drop(replacement);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn identity_check_refuses_a_replacement_socket() {
        let (directory, path) = temporary_socket();
        let original = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let identity = socket_identity(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(matches!(
            remove_matching_socket(&path, identity),
            Err(IpcServerError::UnsafePath(_))
        ));
        assert!(path.exists());
        drop(original);
        drop(replacement);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn production_socket_directory_and_node_are_private() {
        let (base, _) = temporary_socket();
        let directory = base.join("astera");
        ensure_private_directory(&directory).unwrap();
        let socket = directory.join("test.ipc");
        let server = IpcServer::bind_path(socket.clone()).unwrap();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(server);
        fs::remove_dir(directory).unwrap();
        fs::remove_dir(base).unwrap();
    }

    #[test]
    fn request_decoder_still_accepts_the_stream_upgrade_shape() {
        assert!(matches!(
            decode_request("1 (kind:EventStream)\n").unwrap(),
            VersionedRequest::V1(Request {
                kind: RequestKind::EventStream
            })
        ));
    }
}
