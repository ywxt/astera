use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsFd,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use astera_ipc::{
    BOOTSTRAP_VERSION, CURRENT_VERSION, Error, ErrorCode, MIN_VERSION, Request, RequestKind,
    Response, Success, decode_payload, encode_frame, parse_frame, wire,
    wire::v1::{DesktopSnapshot, EventEnvelope},
};
use rustix::{
    buffer::spare_capacity,
    event::epoll::{self, EventData, EventFlags},
    fd::OwnedFd,
    time::Timespec,
};
use smithay::reexports::calloop::{
    EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory, generic::Generic,
};
use thiserror::Error;

use crate::state::Astera;

const LISTENER_KEY: usize = 0;
const MAX_COMMAND_CONNECTIONS: usize = 128;
const MAX_SUBSCRIBERS: usize = 64;
const SUBSCRIBER_QUEUE: usize = 256;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_ACCEPTS_PER_DISPATCH: usize = 16;
const MAX_CONNECTIONS_PER_DISPATCH: usize = 64;
const MAX_FRAMES_PER_CONNECTION: usize = 16;
const MAX_READ_BYTES_PER_CONNECTION: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ConnectionId(u64);

struct PendingCommand {
    connection: ConnectionId,
    response: Response,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionMode {
    Commands,
    Subscriber,
}

struct Connection {
    stream: UnixStream,
    version: Option<u16>,
    input: Vec<u8>,
    discard_oversized: bool,
    output: VecDeque<OutputFrame>,
    output_offset: usize,
    queued_events: usize,
    awaiting_response: bool,
    close_after_write: bool,
    input_closed: bool,
    mode: ConnectionMode,
    last_progress: Instant,
}

struct OutputFrame {
    bytes: Vec<u8>,
    is_event: bool,
}

impl Connection {
    fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            version: None,
            input: Vec::with_capacity(1024),
            discard_oversized: false,
            output: VecDeque::new(),
            output_offset: 0,
            queued_events: 0,
            awaiting_response: false,
            close_after_write: false,
            input_closed: false,
            mode: ConnectionMode::Commands,
            last_progress: Instant::now(),
        })
    }

    fn wants_write(&self) -> bool {
        !self.output.is_empty()
    }

    fn queue(&mut self, frame: String, is_event: bool) {
        if self.output.is_empty() {
            self.last_progress = Instant::now();
        }
        self.output.push_back(OutputFrame {
            bytes: frame.into_bytes(),
            is_event,
        });
    }
}

struct IpcIo {
    listener: Option<UnixListener>,
    poller: Arc<OwnedFd>,
    events: Vec<epoll::Event>,
    connections: BTreeMap<ConnectionId, Connection>,
    requests: VecDeque<(ConnectionId, Request)>,
    next_connection: u64,
    published_sequence: u64,
    path: PathBuf,
}

impl IpcIo {
    fn new(listener: Option<UnixListener>, path: PathBuf) -> io::Result<Self> {
        let poller = Arc::new(epoll::create(epoll::CreateFlags::CLOEXEC)?);
        if let Some(listener) = &listener {
            listener.set_nonblocking(true)?;
            epoll::add(
                &poller,
                listener,
                EventData::new_u64(LISTENER_KEY as u64),
                EventFlags::IN,
            )?;
        }
        Ok(Self {
            listener,
            poller,
            events: Vec::with_capacity(MAX_CONNECTIONS_PER_DISPATCH + 1),
            connections: BTreeMap::new(),
            requests: VecDeque::new(),
            next_connection: 1,
            published_sequence: 0,
            path,
        })
    }

    fn poll_ready(&mut self) {
        self.events.clear();
        let timeout = Timespec::default();
        if let Err(error) = epoll::wait(
            &self.poller,
            spare_capacity(&mut self.events),
            Some(&timeout),
        ) {
            tracing::warn!(%error, path = %self.path.display(), "IPC poll failed");
            return;
        }
        let ready = self.events.clone();
        for event in ready {
            let key = event.data.u64() as usize;
            if key == LISTENER_KEY {
                self.accept_ready();
                continue;
            }
            let id = ConnectionId(key as u64);
            let flags = event.flags;
            let fatal = flags.intersects(EventFlags::ERR | EventFlags::HUP);
            let input_closed = flags.contains(EventFlags::RDHUP);
            let keep = self.service_connection(
                id,
                flags.contains(EventFlags::IN),
                flags.contains(EventFlags::OUT),
                input_closed,
                fatal,
            );
            if !keep {
                self.remove_connection(id);
            }
        }
    }

    fn accept_ready(&mut self) {
        for _ in 0..MAX_ACCEPTS_PER_DISPATCH {
            let accepted = match self
                .listener
                .as_ref()
                .expect("listener event without listener")
                .accept()
            {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    tracing::warn!(%error, path = %self.path.display(), "IPC accept failed");
                    break;
                }
            };
            if !same_uid(&accepted) {
                tracing::warn!(path = %self.path.display(), "rejected IPC peer with different UID");
                continue;
            }
            let command_connections = self
                .connections
                .values()
                .filter(|connection| connection.mode == ConnectionMode::Commands)
                .count();
            if command_connections >= MAX_COMMAND_CONNECTIONS {
                tracing::warn!(path = %self.path.display(), "IPC connection limit reached");
                continue;
            }
            let id = ConnectionId(self.next_connection);
            self.next_connection = self.next_connection.wrapping_add(1).max(1);
            let connection = match Connection::new(accepted) {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!(%error, "could not configure IPC connection");
                    continue;
                }
            };
            if let Err(error) = epoll::add(
                &self.poller,
                &connection.stream,
                EventData::new_u64(id.0),
                EventFlags::IN | EventFlags::RDHUP,
            ) {
                tracing::warn!(%error, ?id, "could not register IPC connection");
                continue;
            }
            self.connections.insert(id, connection);
        }
    }

    fn service_connection(
        &mut self,
        id: ConnectionId,
        readable: bool,
        writable: bool,
        input_closed: bool,
        fatal: bool,
    ) -> bool {
        let Some(mut connection) = self.connections.remove(&id) else {
            return false;
        };
        let mut keep = true;
        if readable && connection.mode == ConnectionMode::Commands {
            keep = self.read_requests(id, &mut connection);
        }
        if keep && writable {
            keep = flush_output(&mut connection);
        }
        if input_closed {
            connection.input_closed = true;
            connection.close_after_write = true;
        }
        if keep && fatal {
            keep = false;
        }
        if keep
            && connection.close_after_write
            && connection.output.is_empty()
            && !connection.awaiting_response
            && !connection.input.contains(&b'\n')
        {
            keep = false;
        }
        if keep && let Err(error) = self.update_interest(id, &connection) {
            tracing::warn!(%error, ?id, "could not update IPC connection interest");
            keep = false;
        }
        if keep {
            self.connections.insert(id, connection);
        } else {
            let _ = epoll::delete(&self.poller, &connection.stream);
        }
        keep
    }

    fn read_requests(&mut self, id: ConnectionId, connection: &mut Connection) -> bool {
        if connection.awaiting_response {
            return true;
        }
        self.parse_buffered_requests(id, connection);
        if connection.awaiting_response || connection.close_after_write {
            return true;
        }
        let mut buffer = [0_u8; 8192];
        let mut read_budget = MAX_READ_BYTES_PER_CONNECTION;
        loop {
            if read_budget == 0 {
                tracing::debug!(
                    ?id,
                    budget = MAX_READ_BYTES_PER_CONNECTION,
                    "IPC read budget exhausted"
                );
                break;
            }
            let capacity = buffer.len().min(read_budget);
            match connection.stream.read(&mut buffer[..capacity]) {
                Ok(0) => {
                    connection.input_closed = true;
                    if connection.awaiting_response || connection.wants_write() {
                        connection.close_after_write = true;
                        return true;
                    }
                    return false;
                }
                Ok(read) => {
                    read_budget -= read;
                    connection.last_progress = Instant::now();
                    connection.input.extend_from_slice(&buffer[..read]);
                    if connection.input.len() > MAX_REQUEST_BYTES
                        && !connection.input.contains(&b'\n')
                    {
                        connection.input.clear();
                        connection.discard_oversized = true;
                    }
                    if read < capacity {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    tracing::warn!(%error, ?id, "IPC read failed");
                    return false;
                }
            }
        }

        self.parse_buffered_requests(id, connection);
        true
    }

    fn parse_buffered_requests(&mut self, id: ConnectionId, connection: &mut Connection) {
        for _ in 0..MAX_FRAMES_PER_CONNECTION {
            if connection.awaiting_response {
                break;
            }
            let Some(newline) = connection.input.iter().position(|byte| *byte == b'\n') else {
                break;
            };
            let line = connection.input.drain(..=newline).collect::<Vec<_>>();
            if connection.discard_oversized || line.len() > MAX_REQUEST_BYTES {
                connection.discard_oversized = false;
                self.queue_frame_error(connection, "IPC request exceeds 65536 bytes".into());
                connection.close_after_write = true;
                break;
            }
            let payload = match String::from_utf8(line) {
                Ok(payload) => payload,
                Err(error) => {
                    self.queue_frame_error(
                        connection,
                        format!("IPC request is not UTF-8: {error}"),
                    );
                    connection.close_after_write = true;
                    break;
                }
            };
            match decode_request_frame(connection, &payload, self.published_sequence) {
                Ok(Some(request)) => {
                    connection.awaiting_response = true;
                    self.requests.push_back((id, request));
                }
                Ok(None) => connection.close_after_write = true,
                Err(response) => {
                    connection.queue(response, false);
                    connection.close_after_write = true;
                }
            }
        }
    }

    fn queue_frame_error(&self, connection: &mut Connection, message: String) {
        let frame = if let Some(version) = connection.version {
            encode_frame(version, &invalid_request(message, self.published_sequence))
        } else {
            encode_frame(
                BOOTSTRAP_VERSION,
                &wire::v0::Response::InvalidFrame { message },
            )
        };
        if let Ok(frame) = frame {
            connection.queue(frame, false);
        }
    }

    fn update_interest(&self, id: ConnectionId, connection: &Connection) -> io::Result<()> {
        let flags = if connection.input_closed && connection.wants_write() {
            EventFlags::OUT
        } else if connection.input_closed {
            EventFlags::empty()
        } else if connection.wants_write() {
            EventFlags::IN | EventFlags::OUT | EventFlags::RDHUP
        } else {
            EventFlags::IN | EventFlags::RDHUP
        };
        Ok(epoll::modify(
            &self.poller,
            &connection.stream,
            EventData::new_u64(id.0),
            flags,
        )?)
    }

    fn queue_response(&mut self, id: ConnectionId, response: Response) {
        let Some(mut connection) = self.connections.remove(&id) else {
            return;
        };
        let version = connection.version.unwrap_or(CURRENT_VERSION);
        if let Ok(frame) = encode_frame(version, &response) {
            connection.queue(frame, false);
            connection.awaiting_response = false;
            if matches!(response, Response::Success(Success::EventStream { .. })) {
                connection.mode = ConnectionMode::Subscriber;
            }
            if connection.mode == ConnectionMode::Commands
                && connection.input.contains(&b'\n')
                && !self.read_requests(id, &mut connection)
            {
                connection.close_after_write = true;
            }
        }
        let _ = self.update_interest(id, &connection);
        self.connections.insert(id, connection);
    }

    fn queue_event(&mut self, id: ConnectionId, event: &EventEnvelope) -> bool {
        let Some(connection) = self.connections.get_mut(&id) else {
            return false;
        };
        if connection.queued_events >= SUBSCRIBER_QUEUE {
            return false;
        }
        let Ok(frame) = encode_frame(CURRENT_VERSION, event) else {
            return false;
        };
        connection.queue(frame, true);
        connection.queued_events += 1;
        let _ = self.update_interest_by_id(id);
        true
    }

    fn update_interest_by_id(&self, id: ConnectionId) -> io::Result<()> {
        let Some(connection) = self.connections.get(&id) else {
            return Ok(());
        };
        self.update_interest(id, connection)
    }

    fn remove_connection(&mut self, id: ConnectionId) {
        if let Some(connection) = self.connections.remove(&id) {
            let _ = epoll::delete(&self.poller, &connection.stream);
        }
    }

    fn expire(&mut self, now: Instant) {
        let expired = self
            .connections
            .iter()
            .filter_map(|(id, connection)| {
                (connection.wants_write()
                    && now.saturating_duration_since(connection.last_progress) >= IO_TIMEOUT)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in expired {
            tracing::warn!(?id, "disconnecting timed-out IPC writer");
            self.remove_connection(id);
        }
    }

    fn next_timeout(&self) -> Option<Instant> {
        self.connections
            .values()
            .filter(|connection| connection.wants_write())
            .map(|connection| connection.last_progress + IO_TIMEOUT)
            .min()
    }
}

fn flush_output(connection: &mut Connection) -> bool {
    while let Some(frame) = connection.output.front() {
        match connection
            .stream
            .write(&frame.bytes[connection.output_offset..])
        {
            Ok(0) => return false,
            Ok(written) => {
                connection.output_offset += written;
                connection.last_progress = Instant::now();
                if connection.output_offset == frame.bytes.len() {
                    let frame = connection
                        .output
                        .pop_front()
                        .expect("front frame existed while completing write");
                    connection.output_offset = 0;
                    if frame.is_event {
                        connection.queued_events -= 1;
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => {
                tracing::warn!(%error, "IPC write failed");
                return false;
            }
        }
    }
    true
}

/// A calloop source that wakes when the IPC listener or any connection becomes ready.
pub struct IpcEventSource {
    source: Generic<Arc<OwnedFd>>,
}

impl EventSource for IpcEventSource {
    type Event = ();
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> io::Result<PostAction>
    where
        F: FnMut(Self::Event, &mut Self::Metadata),
    {
        self.source.process_events(readiness, token, |_, _| {
            callback((), &mut ());
            Ok(PostAction::Continue)
        })
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        tokens: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.source.register(poll, tokens)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        tokens: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.source.reregister(poll, tokens)
    }

    fn unregister(&mut self, poll: &mut Poll) -> smithay::reexports::calloop::Result<()> {
        self.source.unregister(poll)
    }
}

/// Single-threaded IPC protocol manager. Socket readiness comes from [`IpcEventSource`].
pub struct IpcServer {
    io: Rc<RefCell<IpcIo>>,
    commands: Vec<PendingCommand>,
    stream_requests: Vec<ConnectionId>,
    subscribers: BTreeSet<ConnectionId>,
    published_sequence: u64,
    socket_identity: Option<SocketIdentity>,
    pub path: PathBuf,
}

impl IpcServer {
    pub fn bind(display_name: &str) -> Result<Self, IpcServerError> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(IpcServerError::MissingRuntimeDirectory)?;
        let directory = runtime.join("astera");
        ensure_private_directory(&directory)?;
        Self::bind_path(directory.join(format!("{display_name}.ipc")))
    }

    fn bind_path(path: PathBuf) -> Result<Self, IpcServerError> {
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
        let identity = socket_identity(&path)?;
        let io =
            IpcIo::new(Some(listener), path.clone()).map_err(|source| IpcServerError::Socket {
                operation: "initialize event source for",
                path: path.clone(),
                source,
            })?;
        tracing::info!(path = %path.display(), "IPC server is ready");
        Ok(Self {
            io: Rc::new(RefCell::new(io)),
            commands: Vec::new(),
            stream_requests: Vec::new(),
            subscribers: BTreeSet::new(),
            published_sequence: 0,
            socket_identity: Some(identity),
            path,
        })
    }

    pub fn event_source(&self) -> IpcEventSource {
        IpcEventSource {
            source: Generic::new(self.io.borrow().poller.clone(), Interest::READ, Mode::Level),
        }
    }

    pub fn next_timeout(&self) -> Option<Instant> {
        self.io.borrow().next_timeout()
    }

    /// Returns true while a command reply or event still needs to reach a client.
    ///
    /// Backends use this during their bounded shutdown phase so a successful
    /// `Quit` reply is not discarded with the server.
    pub fn has_pending_output(&self) -> bool {
        self.io
            .borrow()
            .connections
            .values()
            .any(Connection::wants_write)
    }

    pub fn expire(&mut self, now: Instant) {
        self.io.borrow_mut().expire(now);
        self.retain_live_subscribers();
    }

    pub fn dispatch(&mut self, state: &mut Astera) {
        self.io.borrow_mut().poll_ready();
        let requests = self.io.borrow_mut().requests.drain(..).collect::<Vec<_>>();
        for (connection, request) in requests {
            match request.kind {
                RequestKind::Command(command) => self.commands.push(PendingCommand {
                    connection,
                    response: state.execute_command_at(command, 0),
                }),
                RequestKind::EventStream => self.stream_requests.push(connection),
            }
        }
    }

    pub fn finish_tick(&mut self, state: &mut Astera) {
        let events = state.publish_public_state().to_vec();
        if state.take_public_sequence_overflow() {
            tracing::warn!("IPC event sequence exhausted; disconnecting all subscribers");
            let subscribers = std::mem::take(&mut self.subscribers);
            for subscriber in subscribers {
                self.io.borrow_mut().remove_connection(subscriber);
            }
        }
        for event in &events {
            let lagged = self
                .subscribers
                .iter()
                .copied()
                .filter(|subscriber| !self.io.borrow_mut().queue_event(*subscriber, event))
                .collect::<Vec<_>>();
            for subscriber in lagged {
                tracing::warn!(
                    sequence = event.sequence,
                    capacity = SUBSCRIBER_QUEUE,
                    "disconnecting lagged IPC event subscriber"
                );
                self.subscribers.remove(&subscriber);
                self.io.borrow_mut().remove_connection(subscriber);
            }
        }

        let sequence = state.public_sequence();
        self.published_sequence = sequence;
        self.io.borrow_mut().published_sequence = sequence;
        let needs_snapshot =
            self.commands.iter().any(|pending| {
                matches!(pending.response, Response::Success(Success::State { .. }))
            }) || !self.stream_requests.is_empty();
        let snapshot = needs_snapshot.then(|| state.public_snapshot());
        for pending in self.commands.drain(..) {
            let response = finalize_response(pending.response, sequence, snapshot.as_ref());
            self.io
                .borrow_mut()
                .queue_response(pending.connection, response);
        }
        for connection in self.stream_requests.drain(..) {
            if self.subscribers.len() >= MAX_SUBSCRIBERS {
                self.io.borrow_mut().queue_response(
                    connection,
                    Response::Error(Error {
                        code: ErrorCode::Conflict,
                        message: "event stream subscriber limit reached".into(),
                        sequence,
                    }),
                );
                continue;
            }
            self.subscribers.insert(connection);
            self.io.borrow_mut().queue_response(
                connection,
                Response::Success(Success::EventStream {
                    sequence,
                    snapshot: snapshot
                        .as_ref()
                        .expect("stream registration requested a snapshot")
                        .clone(),
                }),
            );
        }
        self.retain_live_subscribers();
    }

    fn retain_live_subscribers(&mut self) {
        let io = self.io.borrow();
        self.subscribers
            .retain(|subscriber| io.connections.contains_key(subscriber));
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Some(identity) = self.socket_identity {
            let _ = remove_matching_socket(&self.path, identity);
        }
    }
}

fn decode_request_frame(
    connection: &mut Connection,
    payload: &str,
    sequence: u64,
) -> Result<Option<Request>, String> {
    let frame = parse_frame(payload)
        .map_err(|error| encode_protocol_error(connection, format!("{error}"), sequence))?;
    if connection.version.is_none() && frame.version == BOOTSTRAP_VERSION {
        let response = match decode_payload::<wire::v0::Request>(frame) {
            Ok(_) => wire::v0::Response::Versions {
                minimum: MIN_VERSION,
                current: CURRENT_VERSION,
            },
            Err(error) => wire::v0::Response::InvalidRequest {
                message: error.to_string(),
            },
        };
        let encoded =
            encode_frame(BOOTSTRAP_VERSION, &response).map_err(|error| error.to_string())?;
        connection.queue(encoded, false);
        return Ok(None);
    }
    if frame.version != CURRENT_VERSION {
        return Err(if let Some(expected) = connection.version {
            encode_frame(
                expected,
                &invalid_request(
                    format!(
                        "IPC frame version {} does not match locked version {expected}",
                        frame.version
                    ),
                    sequence,
                ),
            )
        } else {
            encode_frame(
                BOOTSTRAP_VERSION,
                &wire::v0::Response::UnsupportedVersion {
                    requested: frame.version,
                    minimum: MIN_VERSION,
                    current: CURRENT_VERSION,
                },
            )
        }
        .map_err(|error| error.to_string())?);
    }
    if connection
        .version
        .replace(frame.version)
        .is_some_and(|locked| locked != frame.version)
    {
        unreachable!("version mismatch handled before locking")
    }
    decode_payload(frame)
        .map(Some)
        .map_err(|error| encode_protocol_error(connection, error.to_string(), sequence))
}

fn encode_protocol_error(connection: &Connection, message: String, sequence: u64) -> String {
    if let Some(version) = connection.version {
        encode_frame(version, &invalid_request(message, sequence)).unwrap_or_default()
    } else {
        encode_frame(
            BOOTSTRAP_VERSION,
            &wire::v0::Response::InvalidFrame { message },
        )
        .unwrap_or_default()
    }
}

fn invalid_request(message: String, sequence: u64) -> Response {
    Response::Error(Error {
        code: ErrorCode::InvalidRequest,
        message,
        sequence,
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
            snapshot: snapshot.expect("state response requested snapshot").clone(),
        }),
        Response::Success(Success::Handled { .. }) => {
            Response::Success(Success::Handled { sequence })
        }
        Response::Success(Success::EventStream { .. }) => {
            unreachable!("server constructs stream handshake")
        }
        Response::Error(mut error) => {
            error.sequence = sequence;
            error.message.shrink_to_fit();
            Response::Error(error)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> Result<SocketIdentity, IpcServerError> {
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

fn remove_matching_socket(path: &Path, expected: SocketIdentity) -> Result<(), IpcServerError> {
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

fn ensure_private_directory(path: &Path) -> Result<(), IpcServerError> {
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

fn remove_stale_socket(path: &Path) -> Result<(), IpcServerError> {
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
    match UnixStream::connect(path) {
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

fn same_uid(stream: &UnixStream) -> bool {
    rustix::net::sockopt::socket_peercred(stream.as_fd())
        .is_ok_and(|credentials| credentials.uid == rustix::process::getuid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astera_ipc::{Command, decode_frame};
    use smithay::reexports::wayland_server::Display;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::fs::{PermissionsExt, symlink},
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::SystemTime,
    };

    fn temporary_socket() -> (PathBuf, PathBuf) {
        static NEXT_TEMPORARY_SOCKET: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "astera-ipc-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_TEMPORARY_SOCKET.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        let socket = directory.join("test.ipc");
        (directory, socket)
    }

    fn drive(server: &mut IpcServer, state: &mut Astera) {
        server.dispatch(state);
        server.finish_tick(state);
        server.dispatch(state);
    }

    fn server_without_listener() -> IpcServer {
        IpcServer {
            io: Rc::new(RefCell::new(
                IpcIo::new(None, PathBuf::new()).expect("create test epoll"),
            )),
            commands: Vec::new(),
            stream_requests: Vec::new(),
            subscribers: BTreeSet::new(),
            published_sequence: 0,
            socket_identity: None,
            path: PathBuf::new(),
        }
    }

    fn insert_test_connection(server: &mut IpcServer, stream: UnixStream) -> ConnectionId {
        let id = ConnectionId(1);
        let connection = Connection::new(stream).unwrap();
        let mut io = server.io.borrow_mut();
        epoll::add(
            &io.poller,
            &connection.stream,
            EventData::new_u64(id.0),
            EventFlags::IN,
        )
        .unwrap();
        io.connections.insert(id, connection);
        id
    }

    #[test]
    fn command_and_stream_share_an_atomic_sequence_boundary() {
        let (directory, socket) = temporary_socket();
        let mut server = IpcServer::bind_path(socket.clone()).unwrap();
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let client = thread::spawn(move || {
            let mut stream = UnixStream::connect(socket).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let request = encode_frame(
                CURRENT_VERSION,
                &Request {
                    kind: RequestKind::Command(Command::GetState),
                },
            )
            .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut state = String::new();
            reader.read_line(&mut state).unwrap();
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
            let mut stream_response = String::new();
            reader.read_line(&mut stream_response).unwrap();
            result_tx.send((state, stream_response)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            drive(&mut server, &mut state);
            if let Ok((state_frame, stream_frame)) = result_rx.try_recv() {
                let Response::Success(Success::State { sequence, .. }) =
                    decode_frame(&state_frame, CURRENT_VERSION).unwrap()
                else {
                    panic!("expected state")
                };
                let Response::Success(Success::EventStream {
                    sequence: stream_sequence,
                    ..
                }) = decode_frame(&stream_frame, CURRENT_VERSION).unwrap()
                else {
                    panic!("expected stream")
                };
                assert_eq!(sequence, stream_sequence);
                client.join().unwrap();
                drop(server);
                fs::remove_dir(directory).unwrap();
                return;
            }
            thread::yield_now();
        }
        panic!("timed out waiting for IPC exchange")
    }

    #[test]
    fn pipelined_commands_survive_client_write_shutdown() {
        use std::net::Shutdown;

        let (directory, socket) = temporary_socket();
        let mut server = IpcServer::bind_path(socket.clone()).unwrap();
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), astera_config::Config::default());
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let client = thread::spawn(move || {
            let mut stream = UnixStream::connect(socket).unwrap();
            let request = encode_frame(
                CURRENT_VERSION,
                &Request {
                    kind: RequestKind::Command(Command::GetState),
                },
            )
            .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut reader = BufReader::new(stream);
            let mut first = String::new();
            let mut second = String::new();
            reader.read_line(&mut first).unwrap();
            reader.read_line(&mut second).unwrap();
            result_tx.send((first, second)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            drive(&mut server, &mut state);
            if let Ok((first, second)) = result_rx.try_recv() {
                assert!(matches!(
                    decode_frame::<Response>(&first, CURRENT_VERSION).unwrap(),
                    Response::Success(Success::State { .. })
                ));
                assert!(matches!(
                    decode_frame::<Response>(&second, CURRENT_VERSION).unwrap(),
                    Response::Success(Success::State { .. })
                ));
                client.join().unwrap();
                drop(server);
                fs::remove_dir(directory).unwrap();
                return;
            }
            thread::yield_now();
        }
        panic!("timed out waiting for pipelined responses");
    }

    #[test]
    fn version_lock_and_malformed_bootstrap_remain_structured() {
        let (_client, stream) = UnixStream::pair().unwrap();
        let mut connection = Connection::new(stream).unwrap();
        connection.version = Some(CURRENT_VERSION);
        let mismatch = encode_frame(BOOTSTRAP_VERSION, &wire::v0::Request::Versions).unwrap();
        let encoded =
            decode_request_frame(&mut connection, &mismatch, 7).expect_err("version must lock");
        let Response::Error(error) = decode_frame(&encoded, CURRENT_VERSION).unwrap() else {
            panic!("expected structured v1 error")
        };
        assert_eq!(error.sequence, 7);
        assert!(error.message.contains("locked version"));

        let (_client, stream) = UnixStream::pair().unwrap();
        let mut connection = Connection::new(stream).unwrap();
        let malformed = "0 DefinitelyNotVersions\n";
        assert_eq!(
            decode_request_frame(&mut connection, malformed, 0).unwrap(),
            None
        );
        let frame = String::from_utf8(connection.output.pop_front().unwrap().bytes).unwrap();
        assert!(matches!(
            decode_frame::<wire::v0::Response>(&frame, BOOTSTRAP_VERSION).unwrap(),
            wire::v0::Response::InvalidRequest { .. }
        ));
    }

    #[test]
    fn event_source_wakes_for_a_new_connection() {
        let (directory, socket) = temporary_socket();
        let server = IpcServer::bind_path(socket.clone()).unwrap();
        let mut event_loop = smithay::reexports::calloop::EventLoop::<usize>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(server.event_source(), |(), _, wakes| *wakes += 1)
            .unwrap();
        let _client = UnixStream::connect(socket).unwrap();
        let mut wakes = 0;
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 1);
        drop(server);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn future_write_timeout_has_a_real_deadline() {
        let io = IpcIo::new(None, PathBuf::new()).unwrap();
        assert_eq!(io.next_timeout(), None);
    }

    #[test]
    fn handshake_does_not_consume_event_queue_capacity() {
        let (client, stream) = UnixStream::pair().unwrap();
        let mut connection = Connection::new(stream).unwrap();
        connection.mode = ConnectionMode::Subscriber;
        connection.queue("handshake\n".into(), false);
        connection.queue("event\n".into(), true);
        connection.queued_events = 1;

        assert!(flush_output(&mut connection));
        assert_eq!(connection.queued_events, 0);
        drop(client);
    }

    #[test]
    fn subscriber_queue_overflow_disconnects_only_that_connection() {
        let (_client, stream) = UnixStream::pair().unwrap();
        let mut server = server_without_listener();
        let id = insert_test_connection(&mut server, stream);
        server
            .io
            .borrow_mut()
            .connections
            .get_mut(&id)
            .unwrap()
            .mode = ConnectionMode::Subscriber;
        server.subscribers.insert(id);
        let event = EventEnvelope {
            sequence: 1,
            event: wire::v1::Event::Unsupported {
                name: "load".into(),
            },
        };
        for _ in 0..SUBSCRIBER_QUEUE {
            assert!(server.io.borrow_mut().queue_event(id, &event));
        }
        assert!(!server.io.borrow_mut().queue_event(id, &event));
        server.io.borrow_mut().remove_connection(id);
        server.retain_live_subscribers();
        assert!(server.subscribers.is_empty());
    }

    #[test]
    fn blocked_writer_has_a_bounded_timeout() {
        let (_client, stream) = UnixStream::pair().unwrap();
        let mut server = server_without_listener();
        let id = insert_test_connection(&mut server, stream);
        let start = Instant::now();
        {
            let mut io = server.io.borrow_mut();
            let connection = io.connections.get_mut(&id).unwrap();
            connection.queue("x".repeat(8 * 1024 * 1024), false);
            connection.last_progress = start;
            io.update_interest_by_id(id).unwrap();
        }
        assert_eq!(server.next_timeout(), Some(start + IO_TIMEOUT));
        server.expire(start + IO_TIMEOUT);
        assert!(!server.io.borrow().connections.contains_key(&id));
    }

    #[test]
    fn half_closed_slow_reader_does_not_leave_rdhup_armed() {
        use std::net::Shutdown;

        let (client, stream) = UnixStream::pair().unwrap();
        let mut server = server_without_listener();
        let id = insert_test_connection(&mut server, stream);
        {
            let mut io = server.io.borrow_mut();
            let connection = io.connections.get_mut(&id).unwrap();
            connection.queue("x".repeat(8 * 1024 * 1024), false);
            io.update_interest_by_id(id).unwrap();
        }
        client.shutdown(Shutdown::Write).unwrap();
        server.io.borrow_mut().poll_ready();
        {
            let io = server.io.borrow();
            let connection = io.connections.get(&id).unwrap();
            assert!(connection.input_closed);
            assert!(connection.wants_write());
        }

        // The first dispatch drains writes until WouldBlock and changes interest to OUT-only.
        // With the peer intentionally not reading, RDHUP must not wake the inner poller again.
        server.io.borrow_mut().poll_ready();
        assert!(server.io.borrow().events.is_empty());
        drop(client);
    }

    #[test]
    fn stale_cleanup_refuses_regular_files_and_symlinks() {
        let (directory, path) = temporary_socket();
        fs::write(&path, b"keep").unwrap();
        assert!(matches!(
            remove_stale_socket(&path),
            Err(IpcServerError::UnsafePath(_))
        ));
        fs::remove_file(&path).unwrap();
        let target = directory.join("target");
        fs::write(&target, b"keep").unwrap();
        symlink(&target, &path).unwrap();
        assert!(matches!(
            remove_stale_socket(&path),
            Err(IpcServerError::UnsafePath(_))
        ));
        fs::remove_file(path).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn drop_does_not_remove_a_replacement_socket() {
        let (directory, path) = temporary_socket();
        let server = IpcServer::bind_path(path.clone()).unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        drop(server);
        assert!(path.exists());
        drop(replacement);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn production_socket_directory_and_node_are_private() {
        let (base, _) = temporary_socket();
        let directory = base.join("astera");
        ensure_private_directory(&directory).unwrap();
        let path = directory.join("display.ipc");
        let server = IpcServer::bind_path(path.clone()).unwrap();
        assert_eq!(
            directory.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(server);
        fs::remove_dir(directory).unwrap();
        fs::remove_dir(base).unwrap();
    }
}
