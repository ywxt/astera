use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use astera_ipc::{DesktopSnapshot, ErrorCode, PROTOCOL_VERSION, Request, Response};

use crate::state::Astera;

struct PendingRequest {
    request: Request,
    reply: SyncSender<Response<DesktopSnapshot>>,
}

pub struct IpcServer {
    requests: Receiver<PendingRequest>,
    pub path: PathBuf,
}

impl IpcServer {
    pub fn bind(display_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
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
        let (sender, requests) = mpsc::channel();
        let thread_path = path.clone();
        thread::Builder::new()
            .name("astera-ipc".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        break;
                    };
                    if let Err(error) = handle_client(stream, &sender) {
                        tracing::warn!(%error, path = %thread_path.display(), "IPC request failed");
                    }
                }
            })?;
        tracing::info!(path = %path.display(), "IPC server is ready");
        Ok(Self { requests, path })
    }

    pub fn dispatch(&self, state: &mut Astera) {
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
    sender.send(PendingRequest { request, reply })?;
    let response = response.recv()?;
    stream.write_all(ron::to_string(&response)?.as_bytes())?;
    Ok(())
}
