use crate::RunOutput;
use crate::winutil::argv_to_command_line;
use crate::winutil::path_to_wide;
use crate::winutil::win_error;
use anyhow::Result;
use std::ffi::c_void;
use std::fs::File;
use std::io::Read;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr;
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

const WAIT_TIMEOUT: u32 = 0x0000_0102;

struct KillOnCloseJob(HANDLE);

impl KillOnCloseJob {
    fn new() -> Result<Self> {
        let job = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
        if job.is_null() {
            return Err(win_error("CreateJobObjectW"));
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            close(job);
            return Err(win_error("SetInformationJobObject"));
        }
        Ok(Self(job))
    }

    fn assign(&self, process: HANDLE) -> Result<()> {
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            return Err(win_error("AssignProcessToJobObject"));
        }
        Ok(())
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        close(self.0);
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

fn read_pipe(handle: HANDLE) -> thread::JoinHandle<Vec<u8>> {
    let handle_value = handle as usize;
    thread::spawn(move || {
        let handle = handle_value as HANDLE;
        let mut file = unsafe { File::from_raw_handle(handle.cast()) };
        let mut output = Vec::new();
        let _ = file.read_to_end(&mut output);
        output
    })
}

pub fn run_as_user(
    token: HANDLE,
    program: &Path,
    args: &[std::ffi::OsString],
    cwd: &Path,
    environment: &[u16],
    timeout: Option<Duration>,
) -> Result<RunOutput> {
    let mut stdin_read = ptr::null_mut();
    let mut stdin_write = ptr::null_mut();
    let mut stdout_read = ptr::null_mut();
    let mut stdout_write = ptr::null_mut();
    let mut stderr_read = ptr::null_mut();
    let mut stderr_write = ptr::null_mut();
    unsafe {
        if CreatePipe(&mut stdin_read, &mut stdin_write, ptr::null(), 0) == 0 {
            return Err(win_error("CreatePipe stdin"));
        }
        if CreatePipe(&mut stdout_read, &mut stdout_write, ptr::null(), 0) == 0 {
            close(stdin_read);
            close(stdin_write);
            return Err(win_error("CreatePipe stdout"));
        }
        if CreatePipe(&mut stderr_read, &mut stderr_write, ptr::null(), 0) == 0 {
            close(stdin_read);
            close(stdin_write);
            close(stdout_read);
            close(stdout_write);
            return Err(win_error("CreatePipe stderr"));
        }
        for handle in [stdin_read, stdout_write, stderr_write] {
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                close(stdin_read);
                close(stdin_write);
                close(stdout_read);
                close(stdout_write);
                close(stderr_read);
                close(stderr_write);
                return Err(win_error("SetHandleInformation"));
            }
        }
    }

    let child_handles = [stdin_read, stdout_write, stderr_write];
    let mut attributes = match ProcThreadAttributes::new(&child_handles) {
        Ok(value) => value,
        Err(err) => {
            close(stdin_read);
            close(stdin_write);
            close(stdout_read);
            close(stdout_write);
            close(stderr_read);
            close(stderr_write);
            return Err(err);
        }
    };
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_read;
    startup.StartupInfo.hStdOutput = stdout_write;
    startup.StartupInfo.hStdError = stderr_write;
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
        close(stdin_read);
        close(stdin_write);
        close(stdout_read);
        close(stdout_write);
        close(stderr_read);
        close(stderr_write);
        return Err(win_error("CreateProcessAsUserW"));
    }

    // Keep the target process tree bound to a kill-on-close job. The terminal
    // actor can kill the helper wrapper, and the wrapper then closes this job
    // so grandchildren cannot survive a timeout or cancellation.
    let job = match KillOnCloseJob::new() {
        Ok(job) => job,
        Err(error) => {
            unsafe { TerminateProcess(process_info.hProcess, 1) };
            close(process_info.hThread);
            close(process_info.hProcess);
            close(stdin_read);
            close(stdin_write);
            close(stdout_read);
            close(stdout_write);
            close(stderr_read);
            close(stderr_write);
            return Err(error);
        }
    };
    if let Err(error) = job.assign(process_info.hProcess) {
        unsafe { TerminateProcess(process_info.hProcess, 1) };
        let _ = unsafe { WaitForSingleObject(process_info.hProcess, 5_000) };
        close(process_info.hThread);
        close(process_info.hProcess);
        close(stdin_read);
        close(stdin_write);
        close(stdout_read);
        close(stdout_write);
        close(stderr_read);
        close(stderr_write);
        return Err(error);
    }
    if unsafe { ResumeThread(process_info.hThread) } == u32::MAX {
        let error = win_error("ResumeThread");
        unsafe { TerminateProcess(process_info.hProcess, 1) };
        let _ = unsafe { WaitForSingleObject(process_info.hProcess, 5_000) };
        close(process_info.hThread);
        close(process_info.hProcess);
        close(stdin_read);
        close(stdin_write);
        close(stdout_read);
        close(stdout_write);
        close(stderr_read);
        close(stderr_write);
        return Err(error);
    }

    close(process_info.hThread);
    close(stdin_read);
    close(stdin_write);
    close(stdout_write);
    close(stderr_write);
    let stdout_thread = read_pipe(stdout_read);
    let stderr_thread = read_pipe(stderr_read);

    let wait_ms = timeout.map_or(u32::MAX, |duration| {
        duration.as_millis().min(u32::MAX as u128) as u32
    });
    let wait_result = unsafe { WaitForSingleObject(process_info.hProcess, wait_ms) };
    let timed_out = wait_result == WAIT_TIMEOUT;
    if wait_result == u32::MAX {
        unsafe { TerminateProcess(process_info.hProcess, 1) };
        close(process_info.hProcess);
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        return Err(win_error("WaitForSingleObject"));
    }
    if timed_out {
        unsafe { TerminateProcess(process_info.hProcess, 124) };
        let _ = unsafe { WaitForSingleObject(process_info.hProcess, 5_000) };
    }

    let mut exit_code = 1u32;
    let exit_result = unsafe { GetExitCodeProcess(process_info.hProcess, &mut exit_code) };
    close(process_info.hProcess);
    if exit_result == 0 {
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        return Err(win_error("GetExitCodeProcess"));
    }

    Ok(RunOutput {
        exit_code: exit_code as i32,
        stdout: stdout_thread.join().unwrap_or_default(),
        stderr: stderr_thread.join().unwrap_or_default(),
        timed_out,
    })
}

/// Restricted child with live stdin/stdout/stderr pipes. The process is
/// created suspended, attached to a kill-on-close Job Object, and only then
/// resumed, so no child code can run outside the Job boundary.
pub struct RestrictedPipedProcess {
    process: HANDLE,
    _job: KillOnCloseJob,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
}

unsafe impl Send for RestrictedPipedProcess {}

impl RestrictedPipedProcess {
    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    pub fn try_wait(&mut self) -> Result<Option<i32>> {
        let mut exit_code = 0u32;
        if unsafe { GetExitCodeProcess(self.process, &mut exit_code) } == 0 {
            return Err(win_error("GetExitCodeProcess"));
        }
        if exit_code == 259 {
            Ok(None)
        } else {
            Ok(Some(exit_code as i32))
        }
    }

    pub fn wait(&mut self) -> Result<i32> {
        if unsafe { WaitForSingleObject(self.process, u32::MAX) } == u32::MAX {
            return Err(win_error("WaitForSingleObject"));
        }
        self.try_wait()?
            .ok_or_else(|| anyhow::anyhow!("restricted child remained active after wait"))
    }

    pub fn kill(&mut self) -> Result<()> {
        if self.try_wait()?.is_none() {
            if unsafe { TerminateProcess(self.process, 1) } == 0 {
                return Err(win_error("TerminateProcess"));
            }
            let _ = unsafe { WaitForSingleObject(self.process, 5_000) };
        }
        Ok(())
    }
}

impl Drop for RestrictedPipedProcess {
    fn drop(&mut self) {
        let _ = self.kill();
        close(self.process);
    }
}

pub fn spawn_as_user_piped(
    token: HANDLE,
    program: &Path,
    args: &[std::ffi::OsString],
    cwd: &Path,
    environment: &[u16],
) -> Result<RestrictedPipedProcess> {
    let mut stdin_read = ptr::null_mut();
    let mut stdin_write = ptr::null_mut();
    let mut stdout_read = ptr::null_mut();
    let mut stdout_write = ptr::null_mut();
    let mut stderr_read = ptr::null_mut();
    let mut stderr_write = ptr::null_mut();
    unsafe {
        if CreatePipe(&mut stdin_read, &mut stdin_write, ptr::null(), 0) == 0 {
            return Err(win_error("CreatePipe stdin"));
        }
        if CreatePipe(&mut stdout_read, &mut stdout_write, ptr::null(), 0) == 0 {
            close(stdin_read);
            close(stdin_write);
            return Err(win_error("CreatePipe stdout"));
        }
        if CreatePipe(&mut stderr_read, &mut stderr_write, ptr::null(), 0) == 0 {
            close(stdin_read);
            close(stdin_write);
            close(stdout_read);
            close(stdout_write);
            return Err(win_error("CreatePipe stderr"));
        }
        for handle in [stdin_read, stdout_write, stderr_write] {
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                for handle in [
                    stdin_read,
                    stdin_write,
                    stdout_read,
                    stdout_write,
                    stderr_read,
                    stderr_write,
                ] {
                    close(handle);
                }
                return Err(win_error("SetHandleInformation"));
            }
        }
    }

    let child_handles = [stdin_read, stdout_write, stderr_write];
    let mut attributes = ProcThreadAttributes::new(&child_handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_read;
    startup.StartupInfo.hStdOutput = stdout_write;
    startup.StartupInfo.hStdError = stderr_write;
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
        for handle in [
            stdin_read,
            stdin_write,
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
        ] {
            close(handle);
        }
        return Err(win_error("CreateProcessAsUserW"));
    }

    let job = match KillOnCloseJob::new().and_then(|job| {
        job.assign(process_info.hProcess)?;
        Ok(job)
    }) {
        Ok(job) => job,
        Err(error) => {
            unsafe { TerminateProcess(process_info.hProcess, 1) };
            let _ = unsafe { WaitForSingleObject(process_info.hProcess, 5_000) };
            close(process_info.hThread);
            close(process_info.hProcess);
            for handle in [
                stdin_read,
                stdin_write,
                stdout_read,
                stdout_write,
                stderr_read,
                stderr_write,
            ] {
                close(handle);
            }
            return Err(error);
        }
    };
    if unsafe { ResumeThread(process_info.hThread) } == u32::MAX {
        let error = win_error("ResumeThread");
        unsafe { TerminateProcess(process_info.hProcess, 1) };
        let _ = unsafe { WaitForSingleObject(process_info.hProcess, 5_000) };
        close(process_info.hThread);
        close(process_info.hProcess);
        for handle in [
            stdin_read,
            stdin_write,
            stdout_read,
            stdout_write,
            stderr_read,
            stderr_write,
        ] {
            close(handle);
        }
        return Err(error);
    }

    close(process_info.hThread);
    close(stdin_read);
    close(stdout_write);
    close(stderr_write);
    Ok(RestrictedPipedProcess {
        process: process_info.hProcess,
        _job: job,
        stdin: Some(unsafe { File::from_raw_handle(stdin_write.cast()) }),
        stdout: Some(unsafe { File::from_raw_handle(stdout_read.cast()) }),
        stderr: Some(unsafe { File::from_raw_handle(stderr_read.cast()) }),
    })
}
