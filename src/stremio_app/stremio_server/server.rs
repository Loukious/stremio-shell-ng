use crate::stremio_app::constants::{SRV_BUFFER_SIZE, SRV_LOG_SIZE, STREMIO_SERVER_DEV_MODE};
use native_windows_gui::{self as nwg, PartialUi};
use std::io::Write;
use std::{
    env, fs,
    io::Read,
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
}

impl StremioServer {
    pub fn start(&self) {
        if self.development {
            return;
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
            let mut path = env::current_exe()
                .and_then(fs::canonicalize)
                .expect("Cannot get the current executable path");
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
                    let mut stdout = child.stdout.take().unwrap();
                    let out_lines = lines.clone();
                    let tx = tx.clone();
                    let out_thread = thread::spawn(move || {
                        let http_endpoint = String::new();
                        loop {
                            let mut buffer = [0; SRV_BUFFER_SIZE];
                            let on = stdout.read(&mut buffer[..]).unwrap_or(!0);
                            if on > buffer.len() {
                                continue;
                            }
                            std::io::stdout().write_all(&buffer).ok();
                            let string_data = String::from_utf8_lossy(&buffer[..on]);
                            {
                                let lines = &mut *out_lines.lock().unwrap();
                                *lines += string_data.deref();
                                if http_endpoint.is_empty() {
                                    if let Some(http_endpoint) = string_data
                                        .lines()
                                        .find(|line| line.starts_with("EngineFS server started at"))
                                    {
                                        let http_endpoint =
                                            http_endpoint.split_whitespace().last().unwrap();
                                        println!("HTTP endpoint: {http_endpoint}");
                                        let endpoint = http_endpoint.to_string();
                                        tx.send(endpoint.clone()).ok();
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
                            if on == 0 {
                                // Server terminated
                                break;
                            }
                        }
                    });

                    let mut stderr = child.stderr.take().unwrap();
                    let err_lines = lines.clone();
                    let err_thread = thread::spawn(move || {
                        let mut buffer = [0; SRV_BUFFER_SIZE];
                        loop {
                            let en = stderr.read(&mut buffer[..]).unwrap_or(!0);
                            if en > buffer.len() {
                                continue;
                            }
                            std::io::stderr().write_all(&buffer).ok();
                            let string_data = String::from_utf8_lossy(&buffer[..en]);
                            // eprint!("{:?}", &buffer);
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
                            if en == 0 {
                                // Server terminated
                                break;
                            }
                        }
                    });
                    out_thread.join().ok();
                    err_thread.join().ok();
                }
                Err(err) => {
                    nwg::error_message(
                        "Stremio server",
                        format!("Cannot execute stremio-runtime: {}", &err).as_str(),
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

        // Wait for the server to start
        rx.recv().unwrap();
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
        data.start();
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
            self.start();
        }
    }
}
