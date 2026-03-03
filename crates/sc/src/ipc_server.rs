use anyhow::Result;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sc_core::ipc::{Request, Response, StatusResponse, AnalyticsResponse, TuningResponse, SOCKET_PATH};

/// Shared daemon state that the IPC server can read.
pub struct DaemonState {
    pub status: StatusResponse,
    pub analytics: AnalyticsResponse,
    pub tuning: TuningResponse,
}

/// Start the IPC server in a background thread.
pub fn start(state: Arc<Mutex<DaemonState>>) -> Result<()> {
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = UnixListener::bind(SOCKET_PATH)?;
    listener.set_nonblocking(true)?;

    tracing::info!(path = SOCKET_PATH, "IPC server listening");

    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || {
                        if let Err(e) = handle_connection(stream, &state) {
                            tracing::warn!(error = %e, "IPC connection error");
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    tracing::error!(error = %e, "IPC accept error");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });

    Ok(())
}

fn handle_connection(
    stream: std::os::unix::net::UnixStream,
    state: &Arc<Mutex<DaemonState>>,
) -> Result<()> {
    let reader = BufReader::new(&stream);
    let mut writer = &stream;

    for line in reader.lines() {
        let line = line?;
        let request: Request = serde_json::from_str(&line)?;

        let state = state.lock().unwrap();
        let response = match request {
            Request::Status => Response::Status(StatusResponse {
                sensors: state.status.sensors.clone(),
                fans: state.status.fans.clone(),
            }),
            Request::Analytics => Response::Analytics(AnalyticsResponse {
                fans: state.analytics.fans.clone(),
            }),
            Request::Tuning => Response::Tuning(state.tuning.clone()),
        };

        let mut response_json = serde_json::to_string(&response)?;
        response_json.push('\n');
        writer.write_all(response_json.as_bytes())?;
        writer.flush()?;
    }

    Ok(())
}
