use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use astera_ipc::{DesktopSnapshot, ErrorCode, PROTOCOL_VERSION, Request, Response};

use crate::state::Astera;

struct PendingRequest {
    request: Request,
    reply: SyncSender<Response<DesktopSnapshot>>,
    cancelled: Arc<AtomicBool>,
}

pub struct IpcServer {
    requests: Receiver<PendingRequest>,
    pub path: PathBuf,
}

impl IpcServer {
    pub fn bind(display_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or("XDG_RUNTIME_DIR is required for the IPC socket")?;
        let path = runtime.join(format!("{display_name}.ipc"));
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(format!("IPC socket {} is already active", path.display()).into());
                }
                Err(_) => fs::remove_file(&path)?,
            }
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let (sender, requests) = mpsc::channel();
        let thread_path = path.clone();
        let active_clients = Arc::new(AtomicUsize::new(0));
        thread::Builder::new()
            .name("astera-ipc".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        break;
                    };
                    if active_clients.fetch_add(1, Ordering::AcqRel) >= 64 {
                        active_clients.fetch_sub(1, Ordering::AcqRel);
                        tracing::warn!(path = %thread_path.display(), "IPC client limit reached");
                        continue;
                    }
                    let sender = sender.clone();
                    let path = thread_path.clone();
                    let worker_clients = active_clients.clone();
                    if let Err(error) = thread::Builder::new()
                        .name("astera-ipc-client".to_owned())
                        .spawn(move || {
                            if let Err(error) = handle_client(stream, &sender) {
                                tracing::warn!(%error, path = %path.display(), "IPC request failed");
                            }
                            worker_clients.fetch_sub(1, Ordering::AcqRel);
                        })
                    {
                        active_clients.fetch_sub(1, Ordering::AcqRel);
                        tracing::warn!(%error, path = %thread_path.display(), "could not spawn IPC client worker");
                    }
                }
            })?;
        tracing::info!(path = %path.display(), "IPC server is ready");
        Ok(Self { requests, path })
    }

    pub fn dispatch(&self, state: &mut Astera) {
        while let Ok(pending) = self.requests.try_recv() {
            if pending.cancelled.load(Ordering::Acquire) {
                continue;
            }
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
            let _ = pending.reply.send(response);
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn handle_client(
    mut stream: UnixStream,
    sender: &mpsc::Sender<PendingRequest>,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut payload = String::new();
    (&mut stream).take(64 * 1024).read_to_string(&mut payload)?;
    let request = match ron::from_str::<Request>(&payload) {
        Ok(request) => request,
        Err(error) => {
            let response: Response<DesktopSnapshot> = Response::Error {
                code: ErrorCode::InvalidCommand,
                message: error.to_string(),
            };
            stream.write_all(ron::to_string(&response)?.as_bytes())?;
            return Ok(());
        }
    };
    let (reply, response) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    sender.send(PendingRequest {
        request,
        reply,
        cancelled: cancelled.clone(),
    })?;
    let response = match response.recv_timeout(Duration::from_secs(5)) {
        Ok(response) => response,
        Err(error) => {
            cancelled.store(true, Ordering::Release);
            return Err(error.into());
        }
    };
    stream.write_all(ron::to_string(&response)?.as_bytes())?;
    Ok(())
}
