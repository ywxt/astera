use std::{
    fs,
    future::Future,
    io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

use astera_ipc::{DesktopSnapshot, ErrorCode, PROTOCOL_VERSION, Request, Response};
use smol::{
    Executor, Timer,
    io::{AsyncReadExt, AsyncWriteExt},
    net::unix::{UnixListener, UnixStream},
};

use crate::state::Astera;

const MAX_CLIENTS: usize = 64;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

struct PendingRequest {
    request: Request,
    reply: smol::channel::Sender<Response<DesktopSnapshot>>,
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
    pub fn bind(display_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or("XDG_RUNTIME_DIR is required for the IPC socket")?;
        let path = runtime.join(format!("{display_name}.ipc"));
        remove_stale_socket(&path)?;

        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
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
        self.drain_tasks();
        while let Ok(pending) = self.requests.try_recv() {
            let response = if pending.request.version != PROTOCOL_VERSION {
                Response::Error {
                    code: ErrorCode::VersionMismatch,
                    message: format!(
                        "protocol version {} is unsupported; expected {PROTOCOL_VERSION}",
                        pending.request.version
                    ),
                }
            } else {
                state.execute_command(pending.request.command)
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

fn remove_stale_socket(path: &PathBuf) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("IPC socket {} is already active", path.display()),
        )),
        Err(_) => fs::remove_file(path),
    }
}

async fn handle_client(
    mut stream: UnixStream,
    sender: mpsc::SyncSender<PendingRequest>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut payload = String::new();
    with_timeout(
        IO_TIMEOUT,
        (&mut stream)
            .take(MAX_REQUEST_BYTES)
            .read_to_string(&mut payload),
    )
    .await??;
    let request = match ron::from_str::<Request>(&payload) {
        Ok(request) => request,
        Err(error) => {
            let response: Response<DesktopSnapshot> = Response::Error {
                code: ErrorCode::InvalidCommand,
                message: error.to_string(),
            };
            return write_response(&mut stream, response).await;
        }
    };
    let (reply, response) = smol::channel::bounded(1);
    sender.try_send(PendingRequest { request, reply })?;
    let response = with_timeout(Duration::from_secs(5), response.recv()).await??;
    write_response(&mut stream, response).await
}

async fn write_response(
    stream: &mut UnixStream,
    response: Response<DesktopSnapshot>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let payload = ron::to_string(&response)?;
    with_timeout(IO_TIMEOUT, stream.write_all(payload.as_bytes())).await??;
    Ok(())
}

async fn with_timeout<T>(duration: Duration, future: impl Future<Output = T>) -> io::Result<T> {
    smol::future::race(async move { Ok(future.await) }, async move {
        Timer::after(duration).await;
        Err(io::Error::new(io::ErrorKind::TimedOut, "IPC timed out"))
    })
    .await
}
