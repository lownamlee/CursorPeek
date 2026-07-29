use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::{File, OpenOptions},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
    },
    path::PathBuf,
};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

use cursorpeek_core::{payload::VideoPreview, sniff::is_mp4_prefix};

use super::file::{PreviewFile, PreviewFileError};

const MP4_SNIFF_BYTES: usize = 32;
const MAX_VIDEO_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(super) use cursorpeek_core::sniff::is_video_eligible_path as is_eligible_path;

pub(super) fn preview(file: &PreviewFile) -> Result<VideoPreview, VideoPreviewError> {
    if !is_eligible_path(file.final_path()) || file.file_size() == 0 {
        return Err(VideoPreviewError::Unsupported);
    }
    if file.file_size() > MAX_VIDEO_FILE_BYTES {
        return Err(VideoPreviewError::TooLarge);
    }
    let prefix = file.read_prefix(MP4_SNIFF_BYTES)?;
    if !is_mp4_prefix(&prefix) {
        return Err(VideoPreviewError::Unsupported);
    }
    if !file.is_unchanged()? {
        return Err(VideoPreviewError::Changed);
    }
    let path = file.final_path().as_os_str().encode_wide().collect();
    Ok(VideoPreview {
        file_size: file.file_size(),
        last_write_time: file.last_write_time(),
        linked_content: file.is_linked_content(),
        display_name: file.display_name(),
        path,
    })
}

pub(crate) struct PlaybackFileLock {
    _file: File,
}

pub(crate) fn lock_for_playback(path: &[u16]) -> Result<PlaybackFileLock, VideoPreviewError> {
    if path.is_empty() || path.contains(&0) || String::from_utf16(path).is_err() {
        return Err(VideoPreviewError::Unsupported);
    }
    let path = PathBuf::from(OsString::from_wide(path));
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&path)
        .map_err(|source| {
            VideoPreviewError::File(PreviewFileError::Io {
                operation: "lock the video preview path",
                source,
            })
        })?;
    // Re-run the complete local-disk, remote-protocol, offline, path, identity, and MP4 checks
    // while the no-delete lock prevents a path replacement before MFPlay opens the same file.
    let validated = PreviewFile::open(&path)?;
    preview(&validated)?;
    Ok(PlaybackFileLock { _file: file })
}

#[derive(Debug)]
pub(crate) enum VideoPreviewError {
    Unsupported,
    TooLarge,
    Changed,
    File(PreviewFileError),
}

impl From<PreviewFileError> for VideoPreviewError {
    fn from(value: PreviewFileError) -> Self {
        Self::File(value)
    }
}

impl fmt::Display for VideoPreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(formatter, "unsupported MP4 input"),
            Self::TooLarge => write!(formatter, "MP4 exceeds the preview file-size limit"),
            Self::Changed => write!(formatter, "MP4 changed while preparing its preview"),
            Self::File(error) => error.fmt(formatter),
        }
    }
}

impl Error for VideoPreviewError {}
