use crate::stremio_app::constants::{SRV_BUFFER_SIZE, SRV_LOG_SIZE};
use flume::{Receiver, RecvTimeoutError, Sender};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use url::Url;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const READY_PREFIX: &str = "EngineFS server started at ";

#[derive(Debug, PartialEq, Eq)]
pub enum ServerEvent {
    Ready(String),
    Failed(String),
}

enum Message {
    Start,
    Stop,
    Ready(String),
    OutputClosed,
    OutputError(String),
}

type Logs = Arc<Mutex<VecDeque<String>>>;

pub struct ServerProcess {
    sender: Sender<Message>,
    events: Receiver<ServerEvent>,
}

impl ServerProcess {
    pub fn new(
        mut command: impl FnMut() -> Result<Command, String> + Send + 'static,
        timeout: Duration,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let (sender, receiver) = flume::unbounded();
        let (event_sender, events) = flume::unbounded();
        let worker_sender = sender.clone();
        thread::spawn(move || {
            let emit = |event| {
                if event_sender.send(event).is_ok() {
                    notify();
                }
            };
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Start => {
                        let logs = Arc::new(Mutex::new(VecDeque::new()));
                        let result = command().and_then(|mut command| {
                            run_server(
                                &mut command,
                                timeout,
                                &worker_sender,
                                &receiver,
                                &logs,
                                &emit,
                            )
                        });
                        match result {
                            Ok(()) => break,
                            Err(error) => {
                                let logs = logs.lock().unwrap().iter().cloned().collect::<Vec<_>>();
                                let details = if logs.is_empty() {
                                    error
                                } else {
                                    format!("{error}\n\nRecent server output:\n{}", logs.join("\n"))
                                };
                                emit(ServerEvent::Failed(details));
                            }
                        }
                    }
                    Message::Stop => break,
                    // All readers from the previous attempt have finished before retrying.
                    _ => {}
                }
            }
        });
        Self { sender, events }
    }

    pub fn start(&self) {
        self.sender.send(Message::Start).ok();
    }

    pub fn events(&self) -> impl Iterator<Item = ServerEvent> + '_ {
        self.events.try_iter()
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        self.sender.send(Message::Stop).ok();
    }
}

fn run_server(
    command: &mut Command,
    timeout: Duration,
    sender: &Sender<Message>,
    receiver: &Receiver<Message>,
    logs: &Logs,
    emit: &impl Fn(ServerEvent),
) -> Result<(), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot execute stremio-runtime: {error}"))?;

    #[cfg(windows)]
    let job = match super::job::ChildJob::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            child.kill().ok();
            child.wait().ok();
            return Err(format!("Cannot supervise stremio-runtime: {error}"));
        }
    };

    let stdout = child.stdout.take().expect("Piped server stdout");
    let stderr = child.stderr.take().expect("Piped server stderr");
    let stdout_sender = sender.clone();
    let stdout_logs = logs.clone();
    let stdout_thread =
        thread::spawn(move || read_output(stdout, true, &stdout_sender, &stdout_logs));
    let stderr_sender = sender.clone();
    let stderr_logs = logs.clone();
    let stderr_thread =
        thread::spawn(move || read_output(stderr, false, &stderr_sender, &stderr_logs));

    let mut deadline = Some(Instant::now() + timeout);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Err(format!("The streaming server exited ({status}).")),
            Err(error) => {
                break Err(format!(
                    "Cannot read the streaming server exit status: {error}"
                ))
            }
            Ok(None) => {}
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break Err(format!(
                "The streaming server did not start within {} seconds.",
                timeout.as_secs()
            ));
        }

        let wait = deadline
            .map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(POLL_INTERVAL)
            })
            .unwrap_or(POLL_INTERVAL);
        match receiver.recv_timeout(wait) {
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => break Ok(()),
            Ok(Message::Ready(endpoint)) if deadline.take().is_some() => {
                emit(ServerEvent::Ready(endpoint));
            }
            Ok(Message::OutputClosed) if deadline.is_some() => {
                break Err("The streaming server closed its startup output before reporting an HTTP endpoint.".to_string());
            }
            Ok(Message::OutputError(error)) => break Err(error),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
        }
    };

    // Finish cleanup before reporting failure, so Retry cannot overlap two servers.
    #[cfg(windows)]
    drop(job);
    child.kill().ok();
    child.wait().ok();
    stdout_thread.join().ok();
    stderr_thread.join().ok();
    result
}

fn read_output(output: impl Read, stdout: bool, sender: &Sender<Message>, logs: &Logs) {
    for line in BufReader::with_capacity(SRV_BUFFER_SIZE, output).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                let stream = if stdout { "stdout" } else { "stderr" };
                sender
                    .send(Message::OutputError(format!(
                        "Cannot read server {stream}: {error}"
                    )))
                    .ok();
                return;
            }
        };
        if stdout {
            writeln!(std::io::stdout(), "{line}").ok();
        } else {
            writeln!(std::io::stderr(), "{line}").ok();
        }
        {
            let mut logs = logs.lock().unwrap();
            logs.push_back(line.clone());
            if logs.len() > SRV_LOG_SIZE {
                logs.pop_front();
            }
        }
        if stdout {
            if let Some(endpoint) = line.strip_prefix(READY_PREFIX) {
                let message = match parse_endpoint(endpoint) {
                    Some(endpoint) => Message::Ready(endpoint),
                    None => Message::OutputError(format!(
                        "The streaming server reported an invalid HTTP endpoint: {endpoint}"
                    )),
                };
                sender.send(message).ok();
            }
        }
    }
    if stdout {
        sender.send(Message::OutputClosed).ok();
    }
}

fn parse_endpoint(endpoint: &str) -> Option<String> {
    let url = Url::parse(endpoint.trim()).ok()?;
    // The custom runtime may advertise a LAN address. Only its HTTP port is used
    // by this desktop process; the Web UI connects through loopback.
    (url.scheme() == "http"
        && url.host().is_some()
        && url.port().is_some_and(|port| port != 0)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| format!("http://127.0.0.1:{}", url.port().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, path::PathBuf};

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);
    const HTTP_SERVER: &str = r#"
        const server = require('net').createServer();
        server.listen(0, '127.0.0.1', () => {
            console.log('EngineFS server started at http://127.0.0.1:' + server.address().port);
        });
    "#;

    fn runtime() -> PathBuf {
        // The bundled runtime is a native executable, not a Node interpreter.
        PathBuf::from("node")
    }

    fn script_command(script: &str) -> Command {
        let mut command = Command::new(runtime());
        command.args(["-e", script]);
        command
    }

    fn start_script(script: &'static str, timeout: Duration) -> ServerProcess {
        let process = ServerProcess::new(move || Ok(script_command(script)), timeout, || {});
        process.start();
        process
    }

    fn ready(process: &ServerProcess) -> String {
        match process.events.recv_timeout(TEST_TIMEOUT).unwrap() {
            ServerEvent::Ready(endpoint) => endpoint,
            event => panic!("Expected readiness, got {:?}", event),
        }
    }

    fn failed(process: &ServerProcess) -> String {
        match process.events.recv_timeout(TEST_TIMEOUT).unwrap() {
            ServerEvent::Failed(details) => details,
            event => panic!("Expected failure, got {:?}", event),
        }
    }

    fn assert_port_released(endpoint: &str) {
        let port = Url::parse(endpoint).unwrap().port().unwrap();
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            if TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "Server still owns port {}", port);
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn normalizes_advertised_lan_port_and_rejects_invalid_endpoints() {
        assert_eq!(
            parse_endpoint("http://192.168.0.2:11471/base/"),
            Some("http://127.0.0.1:11471".to_owned())
        );
        for endpoint in [
            "not a url",
            "https://127.0.0.1:11470",
            "http://user@127.0.0.1:11470",
            "http://127.0.0.1:0",
        ] {
            assert_eq!(parse_endpoint(endpoint), None);
        }
    }

    #[test]
    fn reports_spawn_failure() {
        let process = ServerProcess::new(
            || Ok(Command::new(runtime().join("missing-runtime.exe"))),
            TEST_TIMEOUT,
            || {},
        );
        process.start();
        assert!(failed(&process).contains("Cannot execute stremio-runtime"));
    }

    #[test]
    fn reads_split_readiness_once() {
        let process = start_script(
            r#"
            process.stdout.write('EngineFS server started at http://127.');
            setTimeout(() => {
                console.log('0.0.1:11471');
                console.log('EngineFS server started at http://127.0.0.1:11472');
            }, 150);
            setInterval(() => {}, 1000);
        "#,
            TEST_TIMEOUT,
        );
        assert_eq!(ready(&process), "http://127.0.0.1:11471");
        assert!(matches!(
            process.events.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn rejects_invalid_readiness() {
        let process = start_script(
            r#"
            console.log('EngineFS server started at http://127.');
            setInterval(() => {}, 1000);
        "#,
            TEST_TIMEOUT,
        );
        assert!(failed(&process).contains("invalid HTTP endpoint"));
    }

    #[test]
    fn reports_exit_before_readiness_with_stderr() {
        let process = start_script(
            r#"
            process.stderr.write('Startup fixture failed\n', () => process.exit(7));
        "#,
            TEST_TIMEOUT,
        );
        let error = failed(&process);
        assert!(error.contains("Startup fixture failed"), "{}", error);
        assert!(
            error.contains("exited") || error.contains("closed its startup output"),
            "{}",
            error
        );
    }

    #[test]
    fn times_out_live_server_and_can_retry() {
        let mut first = true;
        let process = ServerProcess::new(
            move || {
                let script = if first {
                    r#"
                    for (let port = 11470; port <= 11474; port++) {
                        console.error('Error: listen EACCES: permission denied 0.0.0.0:' + port);
                    }
                    setInterval(() => {}, 1000);
                "#
                } else {
                    HTTP_SERVER
                };
                first = false;
                Ok(script_command(script))
            },
            Duration::from_secs(2),
            || {},
        );
        process.start();
        let error = failed(&process);
        assert!(
            error.contains("did not start within 2 seconds"),
            "{}",
            error
        );
        assert!(
            error.contains("EACCES") && error.contains("11474"),
            "{}",
            error
        );
        process.start();
        let endpoint = ready(&process);
        drop(process);
        assert_port_released(&endpoint);
    }

    #[test]
    fn reports_exit_after_readiness() {
        let process = start_script(
            r#"
            console.log('EngineFS server started at http://127.0.0.1:11470');
            setTimeout(() => process.exit(7), 500);
        "#,
            TEST_TIMEOUT,
        );
        ready(&process);
        assert!(failed(&process).contains("exited"));
    }

    #[test]
    fn stopping_releases_the_listener() {
        let process = start_script(HTTP_SERVER, TEST_TIMEOUT);
        let endpoint = ready(&process);
        drop(process);
        assert_port_released(&endpoint);
    }

    #[cfg(windows)]
    #[test]
    fn parent_exit_cleans_up_descendants_with_inherited_output() {
        let script = format!(
            r#"
            require('child_process').spawn(process.execPath, ['-e', {}], {{ stdio: 'inherit' }});
            setTimeout(() => process.exit(7), 1000);
        "#,
            serde_json::to_string(HTTP_SERVER).unwrap()
        );
        let process = ServerProcess::new(move || Ok(script_command(&script)), TEST_TIMEOUT, || {});
        process.start();
        let endpoint = ready(&process);
        assert!(failed(&process).contains("exited"));
        assert_port_released(&endpoint);
    }
}
