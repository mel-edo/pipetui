use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use crossbeam_channel::{unbounded, Receiver, Sender};

#[derive(Clone, Debug)]
pub struct ExecResult {
    pub cmd: String,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub enum WorkerMsg {
    Run(String),
}

pub enum UiMsg {
    Started(String),
    StdoutChunk(String),
    StderrChunk(String),
    Finished(ExecResult),
}

pub fn spawn_worker(rx: Receiver<WorkerMsg>, tx_ui: Sender<UiMsg>) {
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                WorkerMsg::Run(cmd) => {
                    let _ = tx_ui.send(UiMsg::Started(cmd.clone()));

                    #[cfg(target_os = "windows")]
                    let mut command = Command::new("cmd");
                    #[cfg(target_os = "windows")]
                    command.args(["/C", &cmd]);
                    #[cfg(not(target_os = "windows"))]
                    let mut command = Command::new("sh");
                    #[cfg(not(target_os = "windows"))]
                    command.args(["-c", &cmd]);

                    command.stdout(Stdio::piped()).stderr(Stdio::piped());

                    match command.spawn() {
                        Ok(mut child) => {
                            let stdout_log = Arc::new(Mutex::new(String::new()));
                            let stderr_log = Arc::new(Mutex::new(String::new()));

                            let (tx_stdout_chunk, rx_stdout_chunk) = unbounded::<String>();
                            let (tx_stderr_chunk, rx_stderr_chunk) = unbounded::<String>();

                            let stdout_handle = child.stdout.take().map(|stdout| {
                                let tx_chunk = tx_stdout_chunk.clone();
                                let log = Arc::clone(&stdout_log);
                                thread::spawn(move || stream_pipe(stdout, tx_chunk, log))
                            });

                            let stderr_handle = child.stderr.take().map(|stderr| {
                                let tx_chunk = tx_stderr_chunk.clone();
                                let log = Arc::clone(&stderr_log);
                                thread::spawn(move || stream_pipe(stderr, tx_chunk, log))
                            });

                            drop(tx_stdout_chunk);
                            drop(tx_stderr_chunk);

                            let agg_tx = tx_ui.clone();
                            let aggregator = thread::spawn(move || {
                                aggregate_streams(rx_stdout_chunk, rx_stderr_chunk, agg_tx);
                            });

                            let status = child.wait();

                            if let Some(handle) = stdout_handle {
                                let _ = handle.join();
                            }
                            if let Some(handle) = stderr_handle {
                                let _ = handle.join();
                            }
                            let _ = aggregator.join();

                            let status_code = status
                                .as_ref()
                                .ok()
                                .and_then(|s| s.code())
                                .unwrap_or(-1);

                            let stdout = stdout_log
                                .lock()
                                .map(|buf| buf.clone())
                                .unwrap_or_default();
                            let stderr = stderr_log
                                .lock()
                                .map(|buf| buf.clone())
                                .unwrap_or_default();

                            let _ = tx_ui.send(UiMsg::Finished(ExecResult {
                                cmd,
                                status: status_code,
                                stdout,
                                stderr,
                            }));
                        }
                        Err(e) => {
                            let _ = tx_ui.send(UiMsg::Finished(ExecResult {
                                cmd,
                                status: -1,
                                stdout: String::new(),
                                stderr: format!("Failed to spawn: {e}"),
                            }));
                        }
                    }
                }
            }
        }
    });
}

fn stream_pipe(pipe: impl Read, tx: Sender<String>, log: Arc<Mutex<String>>) {
    let mut reader = BufReader::new(pipe);
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let chunk = String::from_utf8_lossy(&buf).to_string();
                if let Ok(mut guard) = log.lock() {
                    guard.push_str(&chunk);
                }
                if tx.send(chunk).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn aggregate_streams(
    rx_stdout: Receiver<String>,
    rx_stderr: Receiver<String>,
    tx_ui: Sender<UiMsg>,
) {
    let ticker = crossbeam_channel::tick(Duration::from_millis(250));
    let mut pending_stdout = String::new();
    let mut pending_stderr = String::new();
    let mut stdout_open = true;
    let mut stderr_open = true;

    loop {
        crossbeam_channel::select! {
            recv(rx_stdout) -> msg => match msg {
                Ok(chunk) => {
                    pending_stdout.push_str(&chunk);
                    continue;
                }
                Err(_) => stdout_open = false,
            },
            recv(rx_stderr) -> msg => match msg {
                Ok(chunk) => {
                    pending_stderr.push_str(&chunk);
                    continue;
                }
                Err(_) => stderr_open = false,
            },
            recv(ticker) -> _ => {},
        }

        if !pending_stdout.is_empty() {
            let chunk = std::mem::take(&mut pending_stdout);
            let _ = tx_ui.send(UiMsg::StdoutChunk(chunk));
        }
        if !pending_stderr.is_empty() {
            let chunk = std::mem::take(&mut pending_stderr);
            let _ = tx_ui.send(UiMsg::StderrChunk(chunk));
        }

        if !stdout_open && !stderr_open {
            if pending_stdout.is_empty() && pending_stderr.is_empty() {
                break;
            }
        }
    }

    if !pending_stdout.is_empty() {
        let _ = tx_ui.send(UiMsg::StdoutChunk(pending_stdout));
    }
    if !pending_stderr.is_empty() {
        let _ = tx_ui.send(UiMsg::StderrChunk(pending_stderr));
    }
}
