// Based on
// https://gitlab.com/tbsaunde/windows-named-pipe/-/blob/f4fd29191f0541f85f818885275dc4573d4059ec/src/lib.rs

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::windows::prelude::OsStrExt;
use std::path::Path;
use std::thread;
use std::time::Duration;
use winapi::shared::minwindef::{DWORD, LPCVOID, LPVOID};
use winapi::shared::winerror;
use winapi::um::fileapi::OPEN_EXISTING;
use winapi::um::fileapi::{CreateFileW, FlushFileBuffers, ReadFile, WriteFile};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::namedpipeapi::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
};
use winapi::um::winbase::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use winapi::um::winnt::{FILE_ATTRIBUTE_NORMAL, GENERIC_READ, GENERIC_WRITE, HANDLE};
#[derive(Debug)]
pub struct PipeClient {
    is_server: bool,
    handle: Handle,
}

impl PipeClient {
    fn create_pipe(path: &Path) -> io::Result<HANDLE> {
        let mut os_str: OsString = path.as_os_str().into();
        os_str.push("\x00");
        let u16_slice = os_str.encode_wide().collect::<Vec<u16>>();

        unsafe { WaitNamedPipeW(u16_slice.as_ptr(), 0) };
        let handle: *mut winapi::ctypes::c_void = unsafe {
            CreateFileW(
                u16_slice.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };

        if handle != INVALID_HANDLE_VALUE {
            Ok(handle)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<PipeClient> {
        let handle = PipeClient::create_pipe(path.as_ref())?;

        Ok(PipeClient {
            handle: Handle { inner: handle },
            is_server: false,
        })
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        unsafe { FlushFileBuffers(self.handle.inner) };
        if self.is_server {
            unsafe { DisconnectNamedPipe(self.handle.inner) };
        }
    }
}

impl Read for PipeClient {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0;
        let ok = unsafe {
            ReadFile(
                self.handle.inner,
                buf.as_mut_ptr() as LPVOID,
                buf.len() as DWORD,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };

        if ok != 0 {
            Ok(bytes_read as usize)
        } else {
            match io::Error::last_os_error().raw_os_error().map(|x| x as u32) {
                Some(winerror::ERROR_PIPE_NOT_CONNECTED) => Ok(0),
                Some(err) => Err(io::Error::from_raw_os_error(err as i32)),
                _ => panic!(""),
            }
        }
    }
}

impl Write for PipeClient {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written = 0;
        let ok = unsafe {
            WriteFile(
                self.handle.inner,
                buf.as_ptr() as LPCVOID,
                buf.len() as DWORD,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };

        if ok != 0 {
            Ok(bytes_written as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let ok = unsafe { FlushFileBuffers(self.handle.inner) };

        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[derive(Debug)]
pub struct PipeServer {
    path: Vec<u16>,
    next_pipe: Handle,
}

fn to_u16s<S: AsRef<OsStr>>(s: S) -> io::Result<Vec<u16>> {
    let mut maybe_result: Vec<u16> = s.as_ref().encode_wide().collect();
    if maybe_result.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "strings passed to WinAPI cannot contain NULs",
        ));
    }
    maybe_result.push(0);
    Ok(maybe_result)
}

impl PipeServer {
    fn create_pipe(path: &[u16], first: bool) -> io::Result<Handle> {
        let mut access_flags = PIPE_ACCESS_DUPLEX;
        if first {
            access_flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let handle = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                access_flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                50,
                std::ptr::null_mut(),
            )
        };

        if handle != INVALID_HANDLE_VALUE {
            Ok(Handle { inner: handle })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn connect_pipe(handle: &Handle) -> io::Result<()> {
        let result = unsafe { ConnectNamedPipe(handle.inner, std::ptr::null_mut()) };

        if result != 0 {
            Ok(())
        } else {
            match io::Error::last_os_error().raw_os_error().map(|x| x as u32) {
                Some(winerror::ERROR_PIPE_CONNECTED) => Ok(()),
                Some(err) => Err(io::Error::from_raw_os_error(err as i32)),
                _ => panic!(""),
            }
        }
    }

    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = to_u16s(path.as_ref().as_os_str())?;
        let next_pipe = PipeServer::create_pipe(&path, true)?;
        Ok(PipeServer { path, next_pipe })
    }

    pub fn accept(&mut self) -> io::Result<PipeClient> {
        let handle = std::mem::replace(
            &mut self.next_pipe,
            PipeServer::create_pipe(&self.path, false)?,
        );

        PipeServer::connect_pipe(&handle)?;

        Ok(PipeClient {
            handle,
            is_server: true,
        })
    }
}

pub enum SingleInstance {
    Forwarded,
    Primary(PipeServer),
    Unavailable(io::Error),
}

// Retry while another instance owns the pipe (bind fails with ERROR_ACCESS_DENIED).
pub fn acquire_single_instance(
    path: &Path,
    command: &[u8],
    attempts: u32,
    delay: Duration,
) -> SingleInstance {
    let mut attempt = 1;
    loop {
        if let Ok(mut stream) = PipeClient::connect(path) {
            match stream.write_all(command).and_then(|_| stream.flush()) {
                Ok(()) => return SingleInstance::Forwarded,
                Err(error) => {
                    eprintln!("Failed to forward command to existing Stremio instance: {error}")
                }
            }
        }
        match PipeServer::bind(path) {
            Ok(server) => return SingleInstance::Primary(server),
            Err(error)
                if attempt < attempts
                    && error.raw_os_error() == Some(winerror::ERROR_ACCESS_DENIED as i32) => {}
            Err(error) => return SingleInstance::Unavailable(error),
        }
        attempt += 1;
        thread::sleep(delay);
    }
}

#[derive(Debug)]
struct Handle {
    inner: HANDLE,
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.inner) };
    }
}

unsafe impl Sync for Handle {}
unsafe impl Send for Handle {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, iter, path::PathBuf, thread, time::Instant};

    #[test]
    fn duplex_communication() {
        let socket_path = Path::new("//./pipe/basicsock");
        println!("{:?}", socket_path);
        let msg1 = b"hello";
        let msg2 = b"world!";

        let mut listener = PipeServer::bind(socket_path).unwrap();
        let thread = thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut buf = [0; 5];
            stream.read(&mut buf).unwrap();
            assert_eq!(&msg1[..], &buf[..]);
            stream.write_all(msg2).unwrap();
        });

        let mut stream = PipeClient::connect(socket_path).unwrap();

        stream.write_all(msg1).unwrap();
        let mut buf = vec![];
        stream.read_to_end(&mut buf).unwrap();
        assert_eq!(&msg2[..], &buf[..]);
        drop(stream);

        thread.join().unwrap();
    }

    const SHORT_DELAY: Duration = Duration::from_millis(20);

    fn pipe_path(name: &str) -> PathBuf {
        PathBuf::from(format!(
            "//./pipe/stremio-test-{}-{name}",
            std::process::id()
        ))
    }

    fn receive(listener: &mut PipeServer) -> Vec<u8> {
        let mut stream = listener.accept().unwrap();
        let mut buf = vec![];
        stream.read_to_end(&mut buf).ok();
        buf
    }

    #[test]
    fn single_instance_becomes_primary_without_owner() {
        let path = pipe_path("primary");
        let mut listener = match acquire_single_instance(&path, b"", 1, SHORT_DELAY) {
            SingleInstance::Primary(listener) => listener,
            _ => panic!("Expected the first launch to become primary"),
        };
        let receiver = thread::spawn(move || receive(&mut listener));
        assert!(matches!(
            acquire_single_instance(&path, b"stremio:///detail", 1, SHORT_DELAY),
            SingleInstance::Forwarded
        ));
        assert_eq!(receiver.join().unwrap(), b"stremio:///detail");
    }

    #[test]
    fn single_instance_retries_until_busy_owner_releases_pipe() {
        let path = pipe_path("released");
        let owner = PipeServer::bind(&path).unwrap();
        let busy = PipeClient::connect(&path).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            drop(busy);
            drop(owner);
        });
        assert!(matches!(
            acquire_single_instance(&path, b"", 50, SHORT_DELAY),
            SingleInstance::Primary(_)
        ));
        release.join().unwrap();
    }

    #[test]
    fn single_instance_stops_after_bounded_attempts() {
        let path = pipe_path("exhausted");
        let _owner = PipeServer::bind(&path).unwrap();
        let _busy = PipeClient::connect(&path).unwrap();
        let started = Instant::now();
        match acquire_single_instance(&path, b"", 3, SHORT_DELAY) {
            SingleInstance::Unavailable(error) => assert_eq!(
                error.raw_os_error(),
                Some(winerror::ERROR_ACCESS_DENIED as i32)
            ),
            _ => panic!("Expected the busy pipe to stay unavailable"),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn single_instance_does_not_retry_other_errors() {
        let path = Path::new("//./not-a-pipe/stremio-test");
        let started = Instant::now();
        match acquire_single_instance(path, b"", 10, Duration::from_secs(1)) {
            SingleInstance::Unavailable(error) => assert_ne!(
                error.raw_os_error(),
                Some(winerror::ERROR_ACCESS_DENIED as i32)
            ),
            _ => panic!("Expected an invalid pipe path to fail"),
        }
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn concurrent_launches_elect_one_primary() {
        const LAUNCHES: usize = 8;
        const DONE: &[u8] = b"done";
        let path = pipe_path("concurrent");
        let launches = (0..LAUNCHES)
            .map(|launch| {
                let path = path.clone();
                thread::spawn(move || {
                    let command = launch.to_string();
                    match acquire_single_instance(&path, command.as_bytes(), 100, SHORT_DELAY) {
                        // Receive on another thread so forwarding launches can finish.
                        SingleInstance::Primary(mut listener) => Some(thread::spawn(move || {
                            let received = iter::repeat_with(|| receive(&mut listener))
                                .take_while(|message| message != DONE)
                                .map(|message| String::from_utf8(message).unwrap())
                                .collect::<HashSet<_>>();
                            (command, received)
                        })),
                        SingleInstance::Forwarded => None,
                        SingleInstance::Unavailable(error) => panic!("{}", error),
                    }
                })
            })
            .collect::<Vec<_>>();
        let results = launches
            .into_iter()
            .map(|launch| launch.join())
            .collect::<Vec<_>>();
        let primaries = results
            .into_iter()
            .filter_map(|result| result.ok().flatten())
            .collect::<Vec<_>>();
        assert_eq!(primaries.len(), 1);
        PipeClient::connect(&path).unwrap().write_all(DONE).unwrap();
        let (primary, received) = primaries.into_iter().next().unwrap().join().unwrap();
        let forwarded = (0..LAUNCHES)
            .map(|launch| launch.to_string())
            .filter(|command| command != &primary)
            .collect::<HashSet<_>>();
        assert_eq!(received, forwarded);
    }
}
