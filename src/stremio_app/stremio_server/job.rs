use once_cell::sync::OnceCell;
use std::{
    io, mem,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::Child,
    ptr,
};
use winapi::um::{
    jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
    processthreadsapi::GetCurrentProcess,
    winnt::{
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

pub struct ChildJob(OwnedHandle);

impl ChildJob {
    fn new(flags: u32) -> io::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(ptr::null_mut(), ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(OwnedHandle::from_raw_handle(handle as _));
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = flags;
            if SetInformationJobObject(
                job.0.as_raw_handle() as _,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *mut _,
                mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }

    /// Closing an attempt's job stops the server and its descendants, not the shell.
    pub fn assign(child: &Child) -> io::Result<Self> {
        let job = Self::new(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)?;
        if unsafe {
            AssignProcessToJobObject(job.0.as_raw_handle() as _, child.as_raw_handle() as _)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

pub fn protect_shell() -> io::Result<()> {
    // Keep the existing shell-wide exit protection, including the interval between
    // spawning a server and assigning it to the attempt's job. Windows closes this
    // static handle at process exit, after the UI has finished dropping.
    static SHELL_JOB: OnceCell<ChildJob> = OnceCell::new();
    SHELL_JOB
        .get_or_try_init(|| {
            let job = ChildJob::new(
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                    | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
            )?;
            if unsafe { AssignProcessToJobObject(job.0.as_raw_handle() as _, GetCurrentProcess()) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        })
        .map(|_| ())
}
