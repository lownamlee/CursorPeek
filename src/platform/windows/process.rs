use std::{
    error::Error,
    ffi::OsStr,
    fmt,
    fs::File,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{
            HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_FAILED,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
                EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
                InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
                PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
                UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    },
    core::{Error as WindowsError, PCWSTR, PWSTR},
};

const WORKER_MEMORY_LIMIT: usize = 384 * 1024 * 1024;
const TERMINATED_EXIT_CODE: u32 = 1;
const CLEANUP_WAIT: Duration = Duration::from_secs(2);

pub(crate) struct WorkerPipes {
    pub(crate) stdin: File,
    pub(crate) stdout: File,
    pub(crate) stderr: File,
}

pub(crate) struct ContainedWorker {
    // Drop the kill-on-close Job before the process handle.
    job: OwnedHandle,
    process: OwnedHandle,
    pipes: Option<WorkerPipes>,
}

impl ContainedWorker {
    pub(crate) fn spawn(executable: &Path) -> Result<Self, ProcessError> {
        let job = create_job()?;
        let stdin = PipePair::for_child_stdin()?;
        let stdout = PipePair::for_child_output()?;
        let stderr = PipePair::for_child_output()?;

        let inherited_handles = [
            as_windows_handle(&stdin.child),
            as_windows_handle(&stdout.child),
            as_windows_handle(&stderr.child),
        ];
        let assigned_jobs = [as_windows_handle(&job)];
        let attributes = AttributeList::new(&inherited_handles, &assigned_jobs)?;

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb =
            u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(|_| ProcessError::SizeOverflow)?;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited_handles[0];
        startup.StartupInfo.hStdOutput = inherited_handles[1];
        startup.StartupInfo.hStdError = inherited_handles[2];
        startup.lpAttributeList = attributes.list;

        let application_name = null_terminated(executable.as_os_str());
        let mut command_line = quoted_worker_command(executable.as_os_str());
        let mut process_information = PROCESS_INFORMATION::default();
        let creation_flags = CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT;

        // SAFETY: every pointer refers to live, correctly sized storage for this call. The
        // application path and mutable command line are NUL-terminated. STARTUPINFOEXW begins
        // with STARTUPINFOW, its cb describes the extended structure, both attribute values stay
        // alive through the call, and the only inheritable handles in the explicit list are the
        // three valid child pipe ends. Process/thread security and environment pointers are null.
        unsafe {
            CreateProcessW(
                PCWSTR(application_name.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                creation_flags,
                None,
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut process_information,
            )?;
        }

        let process = own_handle(process_information.hProcess)?;
        let thread = own_handle(process_information.hThread)?;

        drop(attributes);
        let pipes = WorkerPipes {
            stdin: File::from(stdin.parent),
            stdout: File::from(stdout.parent),
            stderr: File::from(stderr.parent),
        };
        drop(stdin.child);
        drop(stdout.child);
        drop(stderr.child);

        // SAFETY: the thread handle is the owned initial thread returned by successful suspended
        // process creation. The Job was assigned by PROC_THREAD_ATTRIBUTE_JOB_LIST before this
        // call, and all parent copies of the worker pipe ends have already been closed.
        let previous_suspend_count = unsafe { ResumeThread(as_windows_handle(&thread)) };
        if previous_suspend_count != 1 {
            let error = if previous_suspend_count == u32::MAX {
                ProcessError::Native(WindowsError::from_thread())
            } else {
                ProcessError::UnexpectedSuspendCount(previous_suspend_count)
            };
            terminate_and_wait_handles(&job, &process)?;
            return Err(error);
        }
        drop(thread);

        Ok(Self {
            job,
            process,
            pipes: Some(pipes),
        })
    }

    pub(crate) fn take_pipes(&mut self) -> Result<WorkerPipes, ProcessError> {
        self.pipes.take().ok_or(ProcessError::PipesAlreadyTaken)
    }

    pub(crate) fn wait_for_exit(&self, timeout: Duration) -> Result<bool, ProcessError> {
        wait_for_process(&self.process, timeout)
    }

    pub(crate) fn exit_code(&self) -> Result<u32, ProcessError> {
        let mut exit_code = 0;
        // SAFETY: process is a live owned process handle and exit_code points to initialized
        // writable storage for one u32.
        unsafe {
            GetExitCodeProcess(as_windows_handle(&self.process), &mut exit_code)?;
        }
        Ok(exit_code)
    }

    pub(crate) fn terminate_and_wait(&self) -> Result<(), ProcessError> {
        terminate_and_wait_handles(&self.job, &self.process)
    }
}

struct PipePair {
    parent: OwnedHandle,
    child: OwnedHandle,
}

impl PipePair {
    fn for_child_stdin() -> Result<Self, ProcessError> {
        let (read, write) = create_inheritable_pipe()?;
        clear_inheritance(&write)?;
        Ok(Self {
            parent: write,
            child: read,
        })
    }

    fn for_child_output() -> Result<Self, ProcessError> {
        let (read, write) = create_inheritable_pipe()?;
        clear_inheritance(&read)?;
        Ok(Self {
            parent: read,
            child: write,
        })
    }
}

struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Box<[usize]>,
}

impl AttributeList {
    fn new(inherited_handles: &[HANDLE], assigned_jobs: &[HANDLE]) -> Result<Self, ProcessError> {
        let mut required_bytes = 0_usize;
        // SAFETY: a null list is the documented size-query form. required_bytes is valid writable
        // storage, the reserved flags value is zero, and two is the exact maximum attribute count.
        let size_query =
            unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut required_bytes) };
        if required_bytes == 0 {
            return Err(ProcessError::Native(
                size_query.err().unwrap_or_else(WindowsError::from_thread),
            ));
        }

        let word_size = size_of::<usize>();
        let words = required_bytes
            .checked_add(word_size - 1)
            .ok_or(ProcessError::SizeOverflow)?
            / word_size;
        let mut storage = vec![0_usize; words].into_boxed_slice();
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        let mut initialized_bytes = required_bytes;

        // SAFETY: storage is writable, usize-aligned, and at least the byte count returned by the
        // size query. list points to that stable boxed allocation, and initialized_bytes is valid.
        unsafe {
            InitializeProcThreadAttributeList(Some(list), 2, None, &mut initialized_bytes)?;
        }

        let result = (|| {
            // SAFETY: the list was initialized for two entries. Both nonempty slices contain live
            // valid handles and remain alive through CreateProcessW; the reserved/output pointers
            // are null and the byte sizes exactly cover the arrays.
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(inherited_handles.as_ptr().cast()),
                    size_of_val(inherited_handles),
                    None,
                    None,
                )?;
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                    Some(assigned_jobs.as_ptr().cast()),
                    size_of_val(assigned_jobs),
                    None,
                    None,
                )?;
            }
            Ok::<(), WindowsError>(())
        })();

        if let Err(error) = result {
            // SAFETY: the second initialization succeeded, so the opaque list must be deleted
            // once before its backing storage is released.
            unsafe {
                DeleteProcThreadAttributeList(list);
            }
            return Err(ProcessError::Native(error));
        }

        Ok(Self {
            list,
            _storage: storage,
        })
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: construction stores only a successfully initialized list. Drop runs exactly
        // once while the aligned backing allocation is still alive.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

fn create_job() -> Result<OwnedHandle, ProcessError> {
    // SAFETY: no security attributes and no name are supplied, so Windows creates a private
    // non-inheritable Job and returns one owned handle.
    let raw_job = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
    let job = own_handle(raw_job)?;

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = WORKER_MEMORY_LIMIT;
    let limits_size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| ProcessError::SizeOverflow)?;

    // SAFETY: job is a live owned Job handle. limits points to the exact structure required by
    // JobObjectExtendedLimitInformation for limits_size bytes.
    unsafe {
        SetInformationJobObject(
            as_windows_handle(&job),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            limits_size,
        )?;
    }
    Ok(job)
}

fn create_inheritable_pipe() -> Result<(OwnedHandle, OwnedHandle), ProcessError> {
    let security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| ProcessError::SizeOverflow)?,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();

    // SAFETY: read/write point to valid writable handle storage and security is a live,
    // correctly-sized structure requesting inheritable handles with the default descriptor.
    unsafe {
        CreatePipe(&mut read, &mut write, Some(&raw const security), 0)?;
    }
    Ok((own_handle(read)?, own_handle(write)?))
}

fn clear_inheritance(handle: &OwnedHandle) -> Result<(), ProcessError> {
    // SAFETY: handle is a live owned pipe handle. The mask changes only HANDLE_FLAG_INHERIT and
    // zero flags clear it without changing access rights or ownership.
    unsafe {
        SetHandleInformation(
            as_windows_handle(handle),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAGS(0),
        )?;
    }
    Ok(())
}

fn terminate_and_wait_handles(
    job: &OwnedHandle,
    process: &OwnedHandle,
) -> Result<(), ProcessError> {
    if wait_for_process(process, Duration::ZERO)? {
        return Ok(());
    }

    // SAFETY: job is the live private Job that owns this process. Termination is reserved for
    // launch/protocol failure and uses the diagnostic failure exit code.
    if let Err(error) = unsafe { TerminateJobObject(as_windows_handle(job), TERMINATED_EXIT_CODE) }
    {
        if wait_for_process(process, Duration::ZERO)? {
            return Ok(());
        }
        return Err(ProcessError::Native(error));
    }

    if wait_for_process(process, CLEANUP_WAIT)? {
        Ok(())
    } else {
        Err(ProcessError::CleanupTimedOut)
    }
}

fn wait_for_process(process: &OwnedHandle, timeout: Duration) -> Result<bool, ProcessError> {
    let timeout_ms = duration_to_millis(timeout);
    // SAFETY: process is a live owned process handle and timeout_ms is a finite wait.
    let result = unsafe { WaitForSingleObject(as_windows_handle(process), timeout_ms) };
    match result {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(ProcessError::Native(WindowsError::from_thread())),
        other => Err(ProcessError::UnexpectedWaitResult(other.0)),
    }
}

fn duration_to_millis(duration: Duration) -> u32 {
    if duration.is_zero() {
        return 0;
    }
    duration.as_millis().clamp(1, u32::MAX as u128) as u32
}

fn own_handle(handle: HANDLE) -> Result<OwnedHandle, ProcessError> {
    if handle.is_invalid() {
        return Err(ProcessError::InvalidHandle);
    }
    // SAFETY: each caller passes a fresh owned handle returned by a successful Win32 call. This
    // is its single raw-to-owned transition; no other owner will close it.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

fn as_windows_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

fn null_terminated(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn quoted_worker_command(executable: &OsStr) -> Vec<u16> {
    std::iter::once('"' as u16)
        .chain(executable.encode_wide())
        .chain("\" --preview-worker".encode_utf16())
        .chain(std::iter::once(0))
        .collect()
}

#[derive(Debug)]
pub(crate) enum ProcessError {
    Native(WindowsError),
    InvalidHandle,
    SizeOverflow,
    PipesAlreadyTaken,
    UnexpectedSuspendCount(u32),
    UnexpectedWaitResult(u32),
    CleanupTimedOut,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(error) => write!(formatter, "{error}"),
            Self::InvalidHandle => write!(formatter, "Windows returned an invalid owned handle"),
            Self::SizeOverflow => write!(formatter, "native structure size does not fit its field"),
            Self::PipesAlreadyTaken => write!(formatter, "worker pipes were already transferred"),
            Self::UnexpectedSuspendCount(count) => {
                write!(formatter, "initial worker thread had suspend count {count}")
            }
            Self::UnexpectedWaitResult(result) => {
                write!(
                    formatter,
                    "process wait returned unexpected status {result:#x}"
                )
            }
            Self::CleanupTimedOut => {
                write!(
                    formatter,
                    "worker did not terminate within the cleanup deadline"
                )
            }
        }
    }
}

impl Error for ProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            Self::InvalidHandle
            | Self::SizeOverflow
            | Self::PipesAlreadyTaken
            | Self::UnexpectedSuspendCount(_)
            | Self::UnexpectedWaitResult(_)
            | Self::CleanupTimedOut => None,
        }
    }
}

impl From<WindowsError> for ProcessError {
    fn from(error: WindowsError) -> Self {
        Self::Native(error)
    }
}
