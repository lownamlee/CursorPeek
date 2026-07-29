use std::{
    error::Error,
    ffi::OsString,
    fmt,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};

use cursorpeek_core::{payload::VideoPreview, sniff::sniff_video_container};

use crate::preview_file::{PreviewFile, PreviewFileError};

const VIDEO_SNIFF_BYTES: usize = 4 * 1024;
const MAX_VIDEO_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(super) use cursorpeek_core::sniff::is_video_eligible_path as is_eligible_path;

pub(super) fn preview(file: &PreviewFile) -> Result<VideoPreview, VideoPreviewError> {
    if !is_eligible_path(file.final_path()) || file.file_size() == 0 {
        return Err(VideoPreviewError::Unsupported);
    }
    if file.file_size() > MAX_VIDEO_FILE_BYTES {
        return Err(VideoPreviewError::TooLarge);
    }
    let prefix = file.read_prefix(VIDEO_SNIFF_BYTES)?;
    let container =
        sniff_video_container(file.final_path(), &prefix).ok_or(VideoPreviewError::Unsupported)?;
    if !file.is_unchanged()? {
        return Err(VideoPreviewError::Changed);
    }
    let path = file.final_path().as_os_str().encode_wide().collect();
    Ok(VideoPreview {
        file_size: file.file_size(),
        last_write_time: file.last_write_time(),
        volume_serial_number: file.volume_serial_number(),
        file_id: file.file_id(),
        container,
        linked_content: file.is_linked_content(),
        display_name: file.display_name(),
        path,
    })
}

#[cfg(test)]
pub(crate) fn preview_path(path: &std::path::Path) -> Result<VideoPreview, VideoPreviewError> {
    let file = PreviewFile::open(path)?;
    preview(&file)
}

pub(crate) struct PlaybackFileLock {
    _file: PreviewFile,
}

pub(crate) fn lock_for_playback(
    expected: &VideoPreview,
) -> Result<PlaybackFileLock, VideoPreviewError> {
    if expected.path.is_empty()
        || expected.path.contains(&0)
        || String::from_utf16(&expected.path).is_err()
    {
        return Err(VideoPreviewError::Unsupported);
    }
    let path = PathBuf::from(OsString::from_wide(&expected.path));
    // Re-run the complete local-disk, remote-protocol, offline, path, identity, extension, and
    // container checks through the locked handle. Denying write/delete sharing keeps that exact
    // identity stable while path-only MFPlay opens and plays the same local file.
    let locked = PreviewFile::open_locked_for_playback(&path)?;
    let actual = preview(&locked)?;
    if actual.path != expected.path
        || actual.volume_serial_number != expected.volume_serial_number
        || actual.file_id != expected.file_id
        || actual.container != expected.container
        || actual.file_size != expected.file_size
        || actual.last_write_time != expected.last_write_time
    {
        return Err(VideoPreviewError::Changed);
    }
    Ok(PlaybackFileLock { _file: locked })
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
            Self::Unsupported => write!(formatter, "unsupported video input"),
            Self::TooLarge => write!(formatter, "video exceeds the preview file-size limit"),
            Self::Changed => write!(formatter, "video changed while preparing its preview"),
            Self::File(error) => error.fmt(formatter),
        }
    }
}

impl Error for VideoPreviewError {}

#[cfg(test)]
mod tests {
    use super::{VideoPreviewError, lock_for_playback, preview_path};
    use cursorpeek_core::payload::VideoContainer;
    use std::{
        env, fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);
    const MP4_FIXTURE: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";
    const AVI_FIXTURE: &[u8] = b"RIFF\x20\0\0\0AVI LIST";
    const ASF_FIXTURE: &[u8] = &[
        0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce,
        0x6c,
    ];

    #[test]
    fn supported_container_families_are_verified_from_extension_and_header() {
        let root = TestDirectory::new("container-families");
        for (name, bytes, expected) in [
            ("sample.mp4", MP4_FIXTURE, VideoContainer::IsoBaseMedia),
            ("sample.mov", MP4_FIXTURE, VideoContainer::IsoBaseMedia),
            ("sample.avi", AVI_FIXTURE, VideoContainer::Avi),
            ("sample.wmv", ASF_FIXTURE, VideoContainer::Asf),
        ] {
            let path = root.path().join(name);
            fs::write(&path, bytes).expect("the video fixture should be written");
            let preview = preview_path(&path).expect("the container should be accepted");
            assert_eq!(preview.container, expected);
        }

        let disguised = root.path().join("disguised.avi");
        fs::write(&disguised, MP4_FIXTURE).expect("the disguised fixture should be written");
        assert!(matches!(
            preview_path(&disguised),
            Err(VideoPreviewError::Unsupported)
        ));
    }

    #[test]
    fn playback_lock_revalidates_identity_and_denies_replacement() {
        let root = TestDirectory::new("playback-lock");
        let path = root.path().join("sample.mp4");
        let moved = root.path().join("moved.mp4");
        fs::write(&path, MP4_FIXTURE).expect("the MP4 fixture should be written");

        let expected = preview_path(&path).expect("the worker descriptor should be valid");
        let file_lock =
            lock_for_playback(&expected).expect("the same exact file should lock for playback");

        assert!(
            fs::OpenOptions::new().write(true).open(&path).is_err(),
            "the retained playback handle must deny writers"
        );
        assert!(
            fs::rename(&path, &moved).is_err(),
            "the retained playback handle must deny replacement"
        );

        drop(file_lock);
        fs::rename(&path, &moved).expect("replacement should resume after playback stops");
    }

    #[test]
    fn playback_lock_rejects_a_mismatched_worker_descriptor() {
        let root = TestDirectory::new("identity-mismatch");
        let path = root.path().join("sample.mp4");
        fs::write(&path, MP4_FIXTURE).expect("the MP4 fixture should be written");

        let mut expected = preview_path(&path).expect("the worker descriptor should be valid");
        expected.file_id[0] ^= 0xff;

        assert!(matches!(
            lock_for_playback(&expected),
            Err(VideoPreviewError::Changed)
        ));

        let mut expected = preview_path(&path).expect("the worker descriptor should be valid");
        expected.container = VideoContainer::Avi;
        assert!(matches!(
            lock_for_playback(&expected),
            Err(VideoPreviewError::Changed)
        ));
    }

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "cursorpeek-video-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("the test directory should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
