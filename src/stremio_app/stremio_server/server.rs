use crate::stremio_app::constants::{SRV_BUFFER_SIZE, SRV_LOG_SIZE, STREMIO_SERVER_DEV_MODE};
use native_windows_gui::{self as nwg, PartialUi};
use std::{
    env,
    io::{BufRead, BufReader, Read, Write},
    ops::Deref,
    os::windows::process::CommandExt,
    path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};
use winapi::um::{
    processthreadsapi::GetCurrentProcess,
    winbase::{CreateJobObjectA, CREATE_NO_WINDOW},
    winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_BASIC_LIMIT_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

#[derive(Default)]
pub struct StremioServer {
    development: bool,
    parent: nwg::ControlHandle,
    crash_notice: nwg::Notice,
    logs: Arc<Mutex<String>>,
    server_url: Arc<Mutex<Option<String>>>,
}

impl StremioServer {
    pub fn server_url(&self) -> Option<String> {
        self.server_url.lock().ok().and_then(|url| url.clone())
    }

    pub fn start(&self) -> Option<String> {
        if self.development {
            return None;
        }
        let (tx, rx) = flume::unbounded();
        let logs = self.logs.clone();
        let sender = self.crash_notice.sender();

        thread::spawn(move || {
            // Use Win32JobObject to kill the child process when the parent process is killed
            // With the JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK and JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE flags
            unsafe {
                let job_main_process = CreateJobObjectA(std::ptr::null_mut(), std::ptr::null_mut());
                let jeli = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                    BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                        LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                            | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                        ..std::mem::zeroed()
                    },
                    ..std::mem::zeroed()
                };
                winapi::um::jobapi2::SetInformationJobObject(
                    job_main_process,
                    JobObjectExtendedLimitInformation,
                    &jeli as *const _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                winapi::um::jobapi2::AssignProcessToJobObject(
                    job_main_process,
                    GetCurrentProcess(),
                );
            }
            let mut path = env::current_exe().expect("Cannot get the current executable path");
            path.pop();
            let lines = Arc::new(Mutex::new(String::new()));
            let runtime_path = path.clone().join(path::Path::new("stremio-runtime"));
            let server_path = path.clone().join(path::Path::new("server.js"));
            let child = Command::new(runtime_path)
                .arg(server_path)
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            match child {
                Ok(mut child) => {
                    let stdout = child.stdout.take().unwrap();
                    let out_lines = lines.clone();
                    let tx = tx.clone();
                    let out_thread = thread::spawn(move || {
                        let mut stdout = BufReader::new(stdout);
                        let mut endpoint_sent = false;
                        loop {
                            let mut line = String::new();
                            match stdout.read_line(&mut line) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(err) => {
                                    eprintln!("server stdout read error: {err}");
                                    break;
                                }
                            }
                            std::io::stdout().write_all(line.as_bytes()).ok();
                            {
                                let lines = &mut *out_lines.lock().unwrap();
                                *lines += &line;
                                if !endpoint_sent {
                                    if let Some(http_endpoint) = line
                                        .strip_prefix("EngineFS server started at")
                                        .map(str::trim)
                                    {
                                        let endpoint = local_runtime_endpoint(http_endpoint);
                                        println!(
                                            "HTTP endpoint: {endpoint} (runtime advertised {http_endpoint})"
                                        );
                                        tx.send(endpoint.clone()).ok();
                                        endpoint_sent = true;
                                    }
                                }
                                *lines = lines
                                    .lines()
                                    .rev()
                                    .take(SRV_LOG_SIZE)
                                    .collect::<Vec<&str>>()
                                    .into_iter()
                                    .rev()
                                    .collect::<Vec<&str>>()
                                    .join("\n");
                            };
                        }
                    });

                    let mut stderr = child.stderr.take().unwrap();
                    let err_lines = lines.clone();
                    let err_thread = thread::spawn(move || {
                        let mut buffer = [0; SRV_BUFFER_SIZE];
                        loop {
                            let en = match stderr.read(&mut buffer[..]) {
                                Ok(0) => break,
                                Ok(n) => n,
                                Err(err) => {
                                    eprintln!("server stderr read error: {err}");
                                    break;
                                }
                            };
                            std::io::stderr().write_all(&buffer[..en]).ok();
                            let string_data = String::from_utf8_lossy(&buffer[..en]);
                            {
                                let lines = &mut *err_lines.lock().unwrap();
                                *lines += string_data.deref();
                                *lines = lines
                                    .lines()
                                    .rev()
                                    .take(SRV_LOG_SIZE)
                                    .collect::<Vec<&str>>()
                                    .into_iter()
                                    .rev()
                                    .collect::<Vec<&str>>()
                                    .join("\n");
                            };
                        }
                    });
                    out_thread.join().ok();
                    err_thread.join().ok();
                }
                Err(err) => {
                    nwg::error_message(
                        "Stremio server",
                        format!("Cannot execute stremio-runtime: {}", err).as_str(),
                    );
                }
            };

            {
                let mut logs = logs.lock().unwrap();
                *logs = lines.lock().unwrap().deref().to_string();
            }
            println!("Server terminated.");
            sender.notice();
        });

        // The bundled runtime is a child of this desktop process, so the
        // WebUI should always reach it through the local loopback interface.
        let server_url = rx.recv().unwrap();
        if let Ok(mut stored_url) = self.server_url.lock() {
            *stored_url = Some(server_url.clone());
        }
        Some(server_url)
    }
}

fn local_runtime_endpoint(endpoint: &str) -> String {
    let Ok(url) = url::Url::parse(endpoint) else {
        return endpoint.to_string();
    };

    let Some(port) = url.port_or_known_default() else {
        return endpoint.trim_end_matches('/').to_string();
    };

    format!("http://127.0.0.1:{port}")
}

#[cfg(test)]
mod tests {
    use super::local_runtime_endpoint;

    #[test]
    fn child_runtime_endpoint_uses_loopback() {
        assert_eq!(
            local_runtime_endpoint("http://192.168.0.2:11470"),
            "http://127.0.0.1:11470"
        );
    }

    #[test]
    fn child_runtime_endpoint_uses_only_the_advertised_port() {
        assert_eq!(
            local_runtime_endpoint("http://10.0.0.5:11471/base/"),
            "http://127.0.0.1:11471"
        );
    }

    #[test]
    fn invalid_child_runtime_endpoint_is_unchanged() {
        assert_eq!(local_runtime_endpoint("not a url"), "not a url");
    }
}

impl PartialUi for StremioServer {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        if std::env::var(STREMIO_SERVER_DEV_MODE).unwrap_or("false".to_string()) == "true" {
            data.development = true;
        }

        data.parent = parent.expect("No parent window").into();

        nwg::Notice::builder()
            .parent(data.parent)
            .build(&mut data.crash_notice)
            .ok();
        let _ = data.start();
        println!("Stremio server started");
        Ok(())
    }
    fn process_event<'a>(
        &self,
        evt: nwg::Event,
        _evt_data: &nwg::EventData,
        handle: nwg::ControlHandle,
    ) {
        use nwg::Event as E;
        if evt == E::OnNotice && handle == self.crash_notice.handle {
            nwg::modal_error_message(
                self.parent,
                "Stremio server crash log",
                self.logs.lock().unwrap().deref(),
            );
            let _ = self.start();
        }
    }
}
