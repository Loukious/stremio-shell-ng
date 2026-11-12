use super::process::{ServerEvent, ServerProcess};
use crate::stremio_app::constants::STREMIO_SERVER_DEV_MODE;
use native_windows_gui::{self as nwg, PartialUi};
use std::{env, os::windows::process::CommandExt, process::Command, time::Duration};
use winapi::um::winbase::CREATE_NO_WINDOW;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct StremioServer {
    pub notice: nwg::Notice,
    process: Option<ServerProcess>,
}

impl StremioServer {
    pub fn development(&self) -> bool {
        self.process.is_none()
    }

    pub fn start(&self) {
        if let Some(process) = &self.process {
            process.start();
        }
    }

    pub fn events(&self) -> impl Iterator<Item = ServerEvent> + '_ {
        self.process.iter().flat_map(ServerProcess::events)
    }
}

fn server_command() -> Result<Command, String> {
    super::job::protect_shell()
        .map_err(|error| format!("Cannot supervise Stremio child processes: {error}"))?;
    let mut path = env::current_exe()
        .map_err(|error| format!("Cannot locate the Stremio installation: {error}"))?;
    path.pop();
    let mut command = Command::new(path.join("stremio-runtime.exe"));
    command.creation_flags(CREATE_NO_WINDOW);
    Ok(command)
}

impl PartialUi for StremioServer {
    fn build_partial<W: Into<nwg::ControlHandle>>(
        data: &mut Self,
        parent: Option<W>,
    ) -> Result<(), nwg::NwgError> {
        let parent = parent.expect("No parent window").into();
        nwg::Notice::builder()
            .parent(parent)
            .build(&mut data.notice)?;

        if env::var(STREMIO_SERVER_DEV_MODE).as_deref() != Ok("true") {
            let sender = data.notice.sender();
            data.process = Some(ServerProcess::new(
                server_command,
                STARTUP_TIMEOUT,
                move || {
                    sender.notice();
                },
            ));
        }
        Ok(())
    }
}
