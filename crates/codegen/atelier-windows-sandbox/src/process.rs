use crate::winutil::{argv_to_command_line, path_to_wide, to_wide, win_error};
use anyhow::Result;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, DESKTOP_CREATEMENU, DESKTOP_CREATEWINDOW, DESKTOP_DELETE,
    DESKTOP_ENUMERATE, DESKTOP_HOOKCONTROL, DESKTOP_JOURNALPLAYBACK, DESKTOP_JOURNALRECORD,
    DESKTOP_READ_CONTROL, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, DESKTOP_WRITE_DAC,
    DESKTOP_WRITE_OWNER, DESKTOP_WRITEOBJECTS, HDESK,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

const DESKTOP_ALL_ACCESS: u32 = DESKTOP_READOBJECTS
    | DESKTOP_CREATEWINDOW
    | DESKTOP_CREATEMENU
    | DESKTOP_HOOKCONTROL
    | DESKTOP_JOURNALRECORD
    | DESKTOP_JOURNALPLAYBACK
    | DESKTOP_ENUMERATE
    | DESKTOP_WRITEOBJECTS
    | DESKTOP_SWITCHDESKTOP
    | DESKTOP_DELETE
    | DESKTOP_READ_CONTROL
    | DESKTOP_WRITE_DAC
    | DESKTOP_WRITE_OWNER;
static DESKTOP_NONCE: AtomicU64 = AtomicU64::new(1);

struct PrivateDesktop {
    handle: HDESK,
    name: Vec<u16>,
}

impl PrivateDesktop {
    fn create() -> Result<Self> {
        let nonce = DESKTOP_NONCE.fetch_add(1, Ordering::Relaxed);
        let name = to_wide(format!(
            "AtelierSandboxDesktop-{}-{nonce}",
            std::process::id()
        ));
        let handle = unsafe {
            CreateDesktopW(
                name.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                0,
                DESKTOP_ALL_ACCESS,
                ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(win_error("CreateDesktopW"));
        }
        Ok(Self { handle, name })
    }

    fn startup_name(&mut self) -> *mut u16 {
        self.name.as_mut_ptr()
    }
}

impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { CloseDesktop(self.handle) };
        }
    }
}

struct ProcThreadAttributes {
    buffer: Vec<u8>,
    initialized: bool,
}

impl ProcThreadAttributes {
    fn new(handles: &[HANDLE]) -> Result<Self> {
        let mut size = 0usize;
        unsafe {
            let _ = InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(win_error("InitializeProcThreadAttributeList size query"));
        }
        let mut value = Self {
            buffer: vec![0; size],
            initialized: false,
        };
        let list = value.as_ptr();
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(win_error("InitializeProcThreadAttributeList"));
        }
        value.initialized = true;
        if unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                windows_sys::Win32::System::Threading::PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(win_error("UpdateProcThreadAttribute handle list"));
        }
        Ok(value)
    }

    fn as_ptr(&mut self) -> windows_sys::Win32::System::Threading::LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_mut_ptr().cast()
    }
}

impl Drop for ProcThreadAttributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                windows_sys::Win32::System::Threading::DeleteProcThreadAttributeList(
                    self.buffer.as_mut_ptr().cast(),
                );
            }
        }
    }
}

fn close(handle: HANDLE) {
    if !handle.is_null() {
        unsafe { CloseHandle(handle) };
    }
}

/// Restricted child whose standard handles are supplied by the persistent
/// sandbox-user runner. The runner itself is already inside the parent-owned
/// kill-on-close job, so the child inherits that job automatically.
pub struct RunnerChildProcess {
    process: HANDLE,
    _desktop: PrivateDesktop,
}

impl RunnerChildProcess {
    pub fn wait(&mut self) -> Result<i32> {
        if unsafe { WaitForSingleObject(self.process, u32::MAX) } == u32::MAX {
            return Err(win_error("WaitForSingleObject"));
        }
        let mut exit_code = 1u32;
        if unsafe { GetExitCodeProcess(self.process, &mut exit_code) } == 0 {
            return Err(win_error("GetExitCodeProcess"));
        }
        Ok(exit_code as i32)
    }
}

impl Drop for RunnerChildProcess {
    fn drop(&mut self) {
        close(self.process);
    }
}

pub fn spawn_as_user_with_handles(
    token: HANDLE,
    program: &Path,
    args: &[std::ffi::OsString],
    cwd: &Path,
    environment: &[u16],
    stdin: HANDLE,
    stdout: HANDLE,
    stderr: HANDLE,
) -> Result<RunnerChildProcess> {
    let child_handles = [stdin, stdout, stderr];
    for handle in child_handles {
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(win_error("SetHandleInformation(stdio inherit)"));
        }
    }
    let mut attributes = ProcThreadAttributes::new(&child_handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin;
    startup.StartupInfo.hStdOutput = stdout;
    startup.StartupInfo.hStdError = stderr;
    let mut desktop = PrivateDesktop::create()?;
    startup.StartupInfo.lpDesktop = desktop.startup_name();
    startup.lpAttributeList = attributes.as_ptr();

    let app = path_to_wide(program);
    let mut command_line = crate::winutil::to_wide(argv_to_command_line(program, args));
    let cwd_wide = path_to_wide(cwd);
    let mut process_info = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessAsUserW(
            token,
            app.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            1,
            CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT
                | CREATE_NO_WINDOW
                | CREATE_SUSPENDED,
            environment.as_ptr().cast::<c_void>(),
            cwd_wide.as_ptr(),
            &startup.StartupInfo,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(win_error("CreateProcessAsUserW"));
    }
    if unsafe { ResumeThread(process_info.hThread) } == u32::MAX {
        let error = win_error("ResumeThread");
        unsafe { TerminateProcess(process_info.hProcess, 1) };
        close(process_info.hThread);
        close(process_info.hProcess);
        return Err(error);
    }
    close(process_info.hThread);
    Ok(RunnerChildProcess {
        process: process_info.hProcess,
        _desktop: desktop,
    })
}
