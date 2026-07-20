use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::File,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::{
            ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, GENERIC_READ,
            HANDLE,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
            FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
            FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_SEQUENTIAL_SCAN, FILE_ID_INFO,
            FILE_NAME_NORMALIZED, FILE_NAME_OPENED, FILE_REMOTE_PROTOCOL_INFO, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TYPE_DISK,
            FileAttributeTagInfo, FileBasicInfo, FileIdInfo, FileRemoteProtocolInfo,
            FileStandardInfo, GetFileInformationByHandleEx, GetFileType, GetFinalPathNameByHandleW,
            OPEN_EXISTING,
        },
    },
    core::{Error as WindowsError, HRESULT, PCWSTR},
};

const MAX_PATH_UNITS: usize = 32_768;
const INITIAL_FINAL_PATH_UNITS: usize = 260;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshot {
    identity: FileIdentity,
    file_size: u64,
    last_write_time: i64,
    basic_attributes: u32,
    tag_attributes: u32,
    reparse_tag: u32,
}

pub(super) struct PreviewFile {
    file: File,
    opened_path: PathBuf,
    final_path: PathBuf,
    snapshot: FileSnapshot,
}

impl PreviewFile {
    pub(super) fn open(path: &Path) -> Result<Self, PreviewFileError> {
        let path_units = null_terminated_path(path)?;
        if !is_local_drive_path(&path_units[..path_units.len() - 1]) {
            return Err(PreviewFileError::UnsupportedInputPath);
        }
        // SAFETY: `path_units` is a live null-terminated UTF-16 path. The call opens only an
        // existing object, receives no inheritable security attributes or template handle, and
        // explicitly permits ordinary rename/replace activity while this read handle is live.
        let raw_handle = unsafe {
            CreateFileW(
                PCWSTR(path_units.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_OPEN_NO_RECALL,
                None,
            )
        }
        .map_err(|source| PreviewFileError::Windows {
            operation: "open the resolved preview file",
            source,
        })?;

        // SAFETY: CreateFileW returned one valid owned HANDLE. This is its only raw-to-owned
        // transition; `File` closes it exactly once on every later return path.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle.0) };
        let file = File::from(handle);
        require_disk_file(&file)?;
        require_local_protocol(&file)?;
        let snapshot = query_snapshot(&file)?;
        let opened_path = query_handle_path(&file, HandlePathKind::Opened)?;
        let final_path = query_handle_path(&file, HandlePathKind::Normalized)?;

        Ok(Self {
            file,
            opened_path,
            final_path,
            snapshot,
        })
    }

    pub(super) fn is_unchanged(&self) -> Result<bool, PreviewFileError> {
        Ok(query_snapshot(&self.file)? == self.snapshot
            && query_handle_path(&self.file, HandlePathKind::Opened)? == self.opened_path
            && query_handle_path(&self.file, HandlePathKind::Normalized)? == self.final_path)
    }

    #[cfg(test)]
    const fn snapshot(&self) -> FileSnapshot {
        self.snapshot
    }

    #[cfg(test)]
    fn final_path(&self) -> &Path {
        &self.final_path
    }

    #[cfg(test)]
    fn is_linked_content(&self) -> bool {
        self.opened_path != self.final_path
    }
}

fn require_disk_file(file: &File) -> Result<(), PreviewFileError> {
    let file_type = unsafe { GetFileType(file_handle(file)) };
    if file_type == FILE_TYPE_DISK {
        Ok(())
    } else {
        Err(PreviewFileError::NotDisk(file_type.0))
    }
}

fn require_local_protocol(file: &File) -> Result<(), PreviewFileError> {
    const WORDS: usize = size_of::<FILE_REMOTE_PROTOCOL_INFO>() / size_of::<u32>();
    const _: () = assert!(size_of::<FILE_REMOTE_PROTOCOL_INFO>().is_multiple_of(size_of::<u32>()));
    // Keep this infrequently used provider query off the worker stack. A word buffer preserves the
    // native structure's alignment while remaining fully writable to the filesystem provider.
    let mut info = vec![0_u32; WORDS];
    // SAFETY: `info` is the exact output structure paired with FileRemoteProtocolInfo. The file
    // handle remains live and the writable buffer has the reported structure size.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file_handle(file),
            FileRemoteProtocolInfo,
            info.as_mut_ptr().cast(),
            structure_size::<FILE_REMOTE_PROTOCOL_INFO>(),
        )
    };

    match result {
        Ok(()) => Err(PreviewFileError::RemoteProtocol),
        Err(error) if means_no_remote_protocol(&error) => Ok(()),
        Err(source) => Err(PreviewFileError::Windows {
            operation: "determine whether the file uses a remote protocol",
            source,
        }),
    }
}

fn means_no_remote_protocol(error: &WindowsError) -> bool {
    [
        ERROR_INVALID_FUNCTION,
        ERROR_NOT_SUPPORTED,
        ERROR_INVALID_PARAMETER,
    ]
    .into_iter()
    .any(|code| error.code() == HRESULT::from_win32(code.0))
}

fn query_snapshot(file: &File) -> Result<FileSnapshot, PreviewFileError> {
    let standard = query_standard_info(file)?;
    if standard.Directory {
        return Err(PreviewFileError::Directory);
    }
    if standard.DeletePending {
        return Err(PreviewFileError::DeletePending);
    }
    let file_size = u64::try_from(standard.EndOfFile)
        .map_err(|_| PreviewFileError::InvalidFileSize(standard.EndOfFile))?;
    let basic = query_basic_info(file)?;
    let attribute_tag = query_attribute_tag_info(file)?;
    require_content_available(basic.FileAttributes | attribute_tag.FileAttributes)?;
    let id = query_id_info(file)?;

    Ok(FileSnapshot {
        identity: FileIdentity {
            volume_serial_number: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
        },
        file_size,
        last_write_time: basic.LastWriteTime,
        basic_attributes: basic.FileAttributes,
        tag_attributes: attribute_tag.FileAttributes,
        reparse_tag: attribute_tag.ReparseTag,
    })
}

fn query_standard_info(file: &File) -> Result<FILE_STANDARD_INFO, PreviewFileError> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: the live disk handle permits metadata queries. `info` is the exact structure paired
    // with FileStandardInfo and remains writable for its reported size.
    unsafe {
        GetFileInformationByHandleEx(
            file_handle(file),
            FileStandardInfo,
            (&raw mut info).cast(),
            structure_size::<FILE_STANDARD_INFO>(),
        )
    }
    .map_err(|source| PreviewFileError::Windows {
        operation: "query standard file information",
        source,
    })?;
    Ok(info)
}

fn query_basic_info(file: &File) -> Result<FILE_BASIC_INFO, PreviewFileError> {
    let mut info = FILE_BASIC_INFO::default();
    // SAFETY: the live disk handle permits metadata queries. `info` is the exact structure paired
    // with FileBasicInfo and remains writable for its reported size.
    unsafe {
        GetFileInformationByHandleEx(
            file_handle(file),
            FileBasicInfo,
            (&raw mut info).cast(),
            structure_size::<FILE_BASIC_INFO>(),
        )
    }
    .map_err(|source| PreviewFileError::Windows {
        operation: "query basic file information",
        source,
    })?;
    Ok(info)
}

fn query_attribute_tag_info(file: &File) -> Result<FILE_ATTRIBUTE_TAG_INFO, PreviewFileError> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: the live disk handle permits metadata queries. `info` is the exact structure paired
    // with FileAttributeTagInfo and remains writable for its reported size.
    unsafe {
        GetFileInformationByHandleEx(
            file_handle(file),
            FileAttributeTagInfo,
            (&raw mut info).cast(),
            structure_size::<FILE_ATTRIBUTE_TAG_INFO>(),
        )
    }
    .map_err(|source| PreviewFileError::Windows {
        operation: "query file attributes and reparse tag",
        source,
    })?;
    Ok(info)
}

fn require_content_available(attributes: u32) -> Result<(), PreviewFileError> {
    if attributes & FILE_ATTRIBUTE_OFFLINE.0 != 0 {
        Err(PreviewFileError::Offline)
    } else if attributes & FILE_ATTRIBUTE_RECALL_ON_OPEN.0 != 0 {
        Err(PreviewFileError::RecallOnOpen)
    } else if attributes & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0 != 0 {
        Err(PreviewFileError::RecallOnDataAccess)
    } else {
        Ok(())
    }
}

fn query_id_info(file: &File) -> Result<FILE_ID_INFO, PreviewFileError> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the live disk handle permits metadata queries. `info` is the exact structure paired
    // with FileIdInfo and remains writable for its reported size.
    unsafe {
        GetFileInformationByHandleEx(
            file_handle(file),
            FileIdInfo,
            (&raw mut info).cast(),
            structure_size::<FILE_ID_INFO>(),
        )
    }
    .map_err(|source| PreviewFileError::Windows {
        operation: "query stable file identity",
        source,
    })?;
    Ok(info)
}

#[derive(Clone, Copy)]
enum HandlePathKind {
    Opened,
    Normalized,
}

impl HandlePathKind {
    const fn flags(self) -> windows::Win32::Storage::FileSystem::GETFINALPATHNAMEBYHANDLE_FLAGS {
        match self {
            Self::Opened => FILE_NAME_OPENED,
            Self::Normalized => FILE_NAME_NORMALIZED,
        }
    }

    const fn operation(self) -> &'static str {
        match self {
            Self::Opened => "query the opened file path",
            Self::Normalized => "query the normalized file path",
        }
    }
}

fn query_handle_path(file: &File, kind: HandlePathKind) -> Result<PathBuf, PreviewFileError> {
    let mut capacity = INITIAL_FINAL_PATH_UNITS;
    loop {
        let mut units = vec![0_u16; capacity];
        // SAFETY: `units` is a live writable UTF-16 buffer. The handle stays open for the entire
        // query and the selected flag requests one documented DOS-volume path form.
        let returned =
            unsafe { GetFinalPathNameByHandleW(file_handle(file), &mut units, kind.flags()) };
        if returned == 0 {
            return Err(PreviewFileError::Windows {
                operation: kind.operation(),
                source: WindowsError::from_thread(),
            });
        }

        let returned = returned as usize;
        if returned < units.len() {
            units.truncate(returned);
            if units.contains(&0) || !is_extended_drive_path(&units) {
                return Err(PreviewFileError::UnsupportedFinalPath);
            }
            return Ok(PathBuf::from(OsString::from_wide(&units)));
        }
        if returned > MAX_PATH_UNITS {
            return Err(PreviewFileError::PathTooLong(returned));
        }
        capacity = returned;
    }
}

fn null_terminated_path(path: &Path) -> Result<Vec<u16>, PreviewFileError> {
    let mut units = Vec::with_capacity(path.as_os_str().len() + 1);
    for unit in path.as_os_str().encode_wide() {
        if unit == 0 {
            return Err(PreviewFileError::InvalidPath);
        }
        units.push(unit);
        if units.len() == MAX_PATH_UNITS {
            return Err(PreviewFileError::PathTooLong(units.len()));
        }
    }
    units.push(0);
    Ok(units)
}

fn is_local_drive_path(units: &[u16]) -> bool {
    is_drive_path(units) || is_extended_drive_path(units)
}

fn is_drive_path(units: &[u16]) -> bool {
    units.len() >= 3
        && is_ascii_letter(units[0])
        && units[1] == b':' as u16
        && matches!(units[2], 0x2f | 0x5c)
}

fn is_extended_drive_path(units: &[u16]) -> bool {
    units.len() >= 7
        && units[..4] == [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]
        && is_ascii_letter(units[4])
        && units[5] == b':' as u16
        && matches!(units[6], 0x2f | 0x5c)
}

const fn is_ascii_letter(unit: u16) -> bool {
    matches!(unit, 0x41..=0x5a | 0x61..=0x7a)
}

fn file_handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn structure_size<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("Win32 file information structures fit a DWORD")
}

#[derive(Debug)]
pub(super) enum PreviewFileError {
    Windows {
        operation: &'static str,
        source: WindowsError,
    },
    InvalidPath,
    PathTooLong(usize),
    UnsupportedInputPath,
    NotDisk(u32),
    RemoteProtocol,
    Directory,
    DeletePending,
    Offline,
    RecallOnOpen,
    RecallOnDataAccess,
    InvalidFileSize(i64),
    UnsupportedFinalPath,
}

impl PreviewFileError {
    pub(super) const fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::InvalidPath
                | Self::PathTooLong(_)
                | Self::UnsupportedInputPath
                | Self::NotDisk(_)
                | Self::RemoteProtocol
                | Self::Directory
                | Self::Offline
                | Self::RecallOnOpen
                | Self::RecallOnDataAccess
                | Self::InvalidFileSize(_)
                | Self::UnsupportedFinalPath
        )
    }
}

impl fmt::Display for PreviewFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::InvalidPath => write!(formatter, "preview path contains a null character"),
            Self::PathTooLong(length) => {
                write!(formatter, "preview path requires {length} UTF-16 units")
            }
            Self::UnsupportedInputPath => {
                write!(formatter, "preview path is not a local DOS drive path")
            }
            Self::NotDisk(file_type) => write!(formatter, "preview handle type is {file_type}"),
            Self::RemoteProtocol => write!(formatter, "preview file uses a remote protocol"),
            Self::Directory => write!(formatter, "preview target is a directory"),
            Self::DeletePending => write!(formatter, "preview target is pending deletion"),
            Self::Offline => write!(formatter, "preview file data is offline"),
            Self::RecallOnOpen => write!(formatter, "opening the preview file requires recall"),
            Self::RecallOnDataAccess => {
                write!(formatter, "reading the preview file requires recall")
            }
            Self::InvalidFileSize(size) => {
                write!(formatter, "preview file size is invalid ({size})")
            }
            Self::UnsupportedFinalPath => {
                write!(
                    formatter,
                    "preview handle did not resolve to a local DOS drive path"
                )
            }
        }
    }
}

impl Error for PreviewFileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Windows { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PreviewFile, PreviewFileError, is_extended_drive_path, is_local_drive_path,
        null_terminated_path, require_content_available,
    };
    use std::{
        env, fs, io,
        os::windows::ffi::OsStringExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, SetFileAttributesW,
    };
    use windows::core::PCWSTR;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn open_captures_size_identity_and_a_bounded_final_path() {
        let root = TestDirectory::new("identity");
        let path = root.path().join("配置.txt");
        fs::write(&path, b"preview").unwrap();

        let file = PreviewFile::open(&path).expect("the local file should open");
        assert_eq!(file.snapshot().file_size, 7);
        assert_ne!(file.snapshot().identity.file_id, [0; 16]);
        assert!(file.final_path().is_absolute());
        assert!(file.final_path().ends_with("配置.txt"));
        assert!(!file.is_linked_content());
        assert!(file.is_unchanged().unwrap());
    }

    #[test]
    fn share_delete_allows_rename_and_replacement_without_identity_confusion() {
        let root = TestDirectory::new("replacement");
        let original = root.path().join("sample.txt");
        let moved = root.path().join("moved.txt");
        fs::write(&original, b"original").unwrap();

        let first = PreviewFile::open(&original).expect("the original file should open");
        let first_identity = first.snapshot().identity;
        fs::rename(&original, &moved).expect("share-delete should permit a live-handle rename");
        fs::write(&original, b"replacement").unwrap();
        let replacement = PreviewFile::open(&original).expect("the replacement should open");

        assert_ne!(first_identity, replacement.snapshot().identity);
        assert!(!first.is_unchanged().unwrap());
        assert!(replacement.is_unchanged().unwrap());
    }

    #[test]
    fn directories_missing_files_and_embedded_nulls_fail_closed() {
        let root = TestDirectory::new("negative");
        assert!(PreviewFile::open(root.path()).is_err());
        assert!(PreviewFile::open(&root.path().join("missing.txt")).is_err());

        let invalid = PathBuf::from(std::ffi::OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            b'a' as u16,
            0,
            b'b' as u16,
        ]));
        assert!(matches!(
            PreviewFile::open(&invalid),
            Err(PreviewFileError::InvalidPath)
        ));
    }

    #[test]
    fn final_path_shape_accepts_only_extended_dos_drive_paths() {
        assert!(is_extended_drive_path(&wide(r"\\?\C:\file.txt")));
        assert!(is_extended_drive_path(&wide(r"\\?\d:/file.txt")));
        assert!(!is_extended_drive_path(&wide(r"\\?\UNC\server\file")));
        assert!(!is_extended_drive_path(&wide(
            r"\Device\HarddiskVolume1\file"
        )));
        assert!(!is_extended_drive_path(&wide(r"C:\file.txt")));
    }

    #[test]
    fn input_path_shape_rejects_unc_device_and_relative_names_before_open() {
        assert!(is_local_drive_path(&wide(r"C:\file.txt")));
        assert!(is_local_drive_path(&wide(r"d:/file.txt")));
        assert!(is_local_drive_path(&wide(r"\\?\C:\file.txt")));
        assert!(!is_local_drive_path(&wide(r"\\server\share\file.txt")));
        assert!(!is_local_drive_path(&wide(r"\\?\UNC\server\file.txt")));
        assert!(!is_local_drive_path(&wide(r"\\.\C:\file.txt")));
        assert!(!is_local_drive_path(&wide(
            r"\Device\HarddiskVolume1\file.txt"
        )));
        assert!(!is_local_drive_path(&wide(r"relative\file.txt")));

        for path in [
            r"\\server\share\file.txt",
            r"\\?\UNC\server\share\file.txt",
            r"\\.\C:\file.txt",
            r"relative\file.txt",
        ] {
            assert!(matches!(
                PreviewFile::open(Path::new(path)),
                Err(PreviewFileError::UnsupportedInputPath)
            ));
        }
    }

    #[test]
    fn offline_and_recall_attributes_fail_before_content_reads() {
        assert!(matches!(
            require_content_available(FILE_ATTRIBUTE_OFFLINE.0),
            Err(PreviewFileError::Offline)
        ));
        assert!(matches!(
            require_content_available(FILE_ATTRIBUTE_RECALL_ON_OPEN.0),
            Err(PreviewFileError::RecallOnOpen)
        ));
        assert!(matches!(
            require_content_available(FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0),
            Err(PreviewFileError::RecallOnDataAccess)
        ));
        assert!(require_content_available(FILE_ATTRIBUTE_REPARSE_POINT.0).is_ok());
    }

    #[test]
    fn an_offline_file_attribute_is_rejected_from_handle_metadata() {
        let root = TestDirectory::new("offline");
        let path = root.path().join("offline.txt");
        fs::write(&path, b"must not be read").unwrap();
        let units = null_terminated_path(&path).unwrap();
        unsafe { SetFileAttributesW(PCWSTR(units.as_ptr()), FILE_ATTRIBUTE_OFFLINE) }
            .expect("the disposable fixture should accept the offline attribute");

        assert!(matches!(
            PreviewFile::open(&path),
            Err(PreviewFileError::Offline)
        ));
    }

    #[test]
    #[ignore = "requires CURSORPEEK_MAPPED_REMOTE_FILE on a mapped network drive"]
    fn mapped_remote_drive_handle_is_rejected_before_path_acceptance() {
        let path = env::var_os("CURSORPEEK_MAPPED_REMOTE_FILE")
            .map(PathBuf::from)
            .expect("set CURSORPEEK_MAPPED_REMOTE_FILE to an existing mapped-drive file");
        assert!(matches!(
            PreviewFile::open(&path),
            Err(PreviewFileError::RemoteProtocol)
        ));
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            for _ in 0..32 {
                let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = env::temp_dir().join(format!(
                    "cursorpeek-file-{label}-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("test directory `{}` failed: {error}", path.display()),
                }
            }
            panic!("could not reserve a unique file test directory");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap_or_else(|error| {
                panic!("test cleanup `{}` failed: {error}", self.0.display())
            });
        }
    }
}
