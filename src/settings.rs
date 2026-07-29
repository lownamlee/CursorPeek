use std::{
    env,
    error::Error,
    ffi::OsString,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub(crate) use cursorpeek_core::LegacyEncoding;
use cursorpeek_core::protocol::{DEFAULT_PREVIEW_CACHE_ENTRIES, MAX_PREVIEW_CACHE_ENTRIES};
use windows::{
    Win32::{
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath},
    },
    core::{Error as WindowsError, PCWSTR, PWSTR},
};

const APPLICATION_DIRECTORY: &str = "CursorPeek";
const CONFIG_FILE_NAME: &str = "config.ini";
const PORTABLE_MARKER_NAME: &str = "CursorPeek.portable";
const MAX_CONFIG_BYTES: usize = 32 * 1024;
const MAX_LINE_BYTES: usize = 1_024;
const MAX_KEY_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 1_024;
const MAX_UNKNOWN_SETTINGS: usize = 64;
const MAX_KNOWN_FOLDER_UNITS: usize = 32_767;
const TEMPORARY_FILE_ATTEMPTS: usize = 32;

const MIN_DWELL_DELAY_MS: u64 = 50;
const MAX_DWELL_DELAY_MS: u64 = 2_000;
const MIN_PREVIEW_WIDTH: u16 = 320;
const MAX_PREVIEW_WIDTH: u16 = 960;
const MIN_PREVIEW_HEIGHT: u16 = 240;
const MAX_PREVIEW_HEIGHT: u16 = 720;
static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsMode {
    Installed,
    Portable,
}

impl SettingsMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Portable => "portable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[cfg(feature = "diagnostic-log")]
pub(crate) fn diagnostics_directory() -> Result<PathBuf, SettingsError> {
    Ok(current_local_app_data()?
        .join(APPLICATION_DIRECTORY)
        .join("diagnostics"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppSettings {
    dwell_delay_ms: u64,
    preview_width: u16,
    preview_height: u16,
    cache_entries: u16,
    theme: Theme,
    legacy_encoding: LegacyEncoding,
    start_with_windows: bool,
    video_previews: bool,
    video_audio: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            dwell_delay_ms: 50,
            preview_width: 640,
            preview_height: 480,
            cache_entries: DEFAULT_PREVIEW_CACHE_ENTRIES,
            theme: Theme::System,
            legacy_encoding: LegacyEncoding::Auto,
            start_with_windows: false,
            video_previews: true,
            video_audio: false,
        }
    }
}

impl AppSettings {
    pub(crate) const fn dwell_delay(&self) -> Duration {
        Duration::from_millis(self.dwell_delay_ms)
    }

    pub(crate) const fn dwell_delay_ms(&self) -> u64 {
        self.dwell_delay_ms
    }

    pub(crate) const fn legacy_encoding(&self) -> &LegacyEncoding {
        &self.legacy_encoding
    }

    pub(crate) const fn preview_width(&self) -> u16 {
        self.preview_width
    }

    pub(crate) const fn preview_height(&self) -> u16 {
        self.preview_height
    }

    pub(crate) const fn cache_entries(&self) -> u16 {
        self.cache_entries
    }

    pub(crate) const fn theme(&self) -> Theme {
        self.theme
    }

    pub(crate) const fn start_with_windows(&self) -> bool {
        self.start_with_windows
    }

    pub(crate) const fn video_previews(&self) -> bool {
        self.video_previews
    }

    pub(crate) const fn video_audio(&self) -> bool {
        self.video_audio
    }

    fn validate(&self) -> Result<(), SettingsParseError> {
        if !(MIN_DWELL_DELAY_MS..=MAX_DWELL_DELAY_MS).contains(&self.dwell_delay_ms) {
            return Err(SettingsParseError::new(0, INVALID_DWELL_DELAY));
        }
        if !(MIN_PREVIEW_WIDTH..=MAX_PREVIEW_WIDTH).contains(&self.preview_width) {
            return Err(SettingsParseError::new(0, INVALID_PREVIEW_WIDTH));
        }
        if !(MIN_PREVIEW_HEIGHT..=MAX_PREVIEW_HEIGHT).contains(&self.preview_height) {
            return Err(SettingsParseError::new(0, INVALID_PREVIEW_HEIGHT));
        }
        if self.cache_entries > MAX_PREVIEW_CACHE_ENTRIES {
            return Err(SettingsParseError::new(0, INVALID_CACHE_ENTRIES));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnknownSetting {
    key: String,
    value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SettingsDocument {
    settings: AppSettings,
    unknown: Vec<UnknownSetting>,
}

impl SettingsDocument {
    pub(crate) const fn settings(&self) -> &AppSettings {
        &self.settings
    }

    pub(crate) fn set_dwell_delay_ms(
        &mut self,
        dwell_delay_ms: u64,
    ) -> Result<(), SettingsParseError> {
        let previous = self.settings.dwell_delay_ms;
        self.settings.dwell_delay_ms = dwell_delay_ms;
        if let Err(error) = self.settings.validate() {
            self.settings.dwell_delay_ms = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn set_preview_size(
        &mut self,
        preview_width: u16,
        preview_height: u16,
    ) -> Result<(), SettingsParseError> {
        let previous = (self.settings.preview_width, self.settings.preview_height);
        self.settings.preview_width = preview_width;
        self.settings.preview_height = preview_height;
        if let Err(error) = self.settings.validate() {
            (self.settings.preview_width, self.settings.preview_height) = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn set_start_with_windows(&mut self, start_with_windows: bool) {
        self.settings.start_with_windows = start_with_windows;
    }

    pub(crate) fn set_video_previews(&mut self, enabled: bool) {
        self.settings.video_previews = enabled;
    }

    pub(crate) fn set_video_audio(&mut self, enabled: bool) {
        self.settings.video_audio = enabled;
    }

    pub(crate) fn set_theme(&mut self, theme: Theme) {
        self.settings.theme = theme;
    }

    fn parse(text: &str) -> Result<Self, SettingsParseError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut document = Self::default();
        let mut keys = Vec::new();

        for (index, raw_line) in text.lines().enumerate() {
            let line_number = index + 1;
            if raw_line.len() > MAX_LINE_BYTES {
                return Err(SettingsParseError::new(line_number, LINE_TOO_LONG));
            }

            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                SettingsParseError::new(line_number, "expected one key=value pair")
            })?;
            let key = key.trim();
            let value = value.trim();
            validate_key(key, line_number)?;
            validate_value(value, line_number)?;

            if keys.contains(&key) {
                return Err(SettingsParseError::new(line_number, DUPLICATE_KEY));
            }
            keys.push(key);

            match key {
                "dwell_delay_ms" => {
                    document.settings.dwell_delay_ms = parse_bounded_integer(
                        value,
                        MIN_DWELL_DELAY_MS,
                        MAX_DWELL_DELAY_MS,
                        line_number,
                        INVALID_DWELL_DELAY,
                    )?;
                }
                "preview_width" => {
                    document.settings.preview_width = u16::try_from(parse_bounded_integer(
                        value,
                        u64::from(MIN_PREVIEW_WIDTH),
                        u64::from(MAX_PREVIEW_WIDTH),
                        line_number,
                        INVALID_PREVIEW_WIDTH,
                    )?)
                    .expect("the validated width fits u16");
                }
                "preview_height" => {
                    document.settings.preview_height = u16::try_from(parse_bounded_integer(
                        value,
                        u64::from(MIN_PREVIEW_HEIGHT),
                        u64::from(MAX_PREVIEW_HEIGHT),
                        line_number,
                        INVALID_PREVIEW_HEIGHT,
                    )?)
                    .expect("the validated height fits u16");
                }
                "cache_entries" => {
                    document.settings.cache_entries = u16::try_from(parse_bounded_integer(
                        value,
                        0,
                        u64::from(MAX_PREVIEW_CACHE_ENTRIES),
                        line_number,
                        INVALID_CACHE_ENTRIES,
                    )?)
                    .expect("the validated cache entry limit fits u16");
                }
                "theme" => {
                    document.settings.theme = Theme::parse(value)
                        .ok_or_else(|| SettingsParseError::new(line_number, INVALID_THEME))?;
                }
                "legacy_encoding" => {
                    document.settings.legacy_encoding =
                        LegacyEncoding::parse(value).ok_or_else(|| {
                            SettingsParseError::new(line_number, INVALID_LEGACY_ENCODING)
                        })?;
                }
                "start_with_windows" => {
                    document.settings.start_with_windows =
                        parse_boolean(value, line_number, INVALID_START_WITH_WINDOWS)?;
                }
                "video_previews" => {
                    document.settings.video_previews =
                        parse_boolean(value, line_number, INVALID_VIDEO_PREVIEWS)?;
                }
                "video_audio" => {
                    document.settings.video_audio =
                        parse_boolean(value, line_number, INVALID_VIDEO_AUDIO)?;
                }
                "video_smooth_start" => {
                    // Versions that exposed the fixed preroll wrote this key. Validate and
                    // intentionally discard it so the next save removes the obsolete delay.
                    let _ = parse_boolean(value, line_number, INVALID_VIDEO_SMOOTH_START)?;
                }
                _ => {
                    if document.unknown.len() == MAX_UNKNOWN_SETTINGS {
                        return Err(SettingsParseError::new(line_number, TOO_MANY_UNKNOWN));
                    }
                    document.unknown.push(UnknownSetting {
                        key: key.to_owned(),
                        value: value.to_owned(),
                    });
                }
            }
        }

        document.settings.validate()?;
        Ok(document)
    }

    fn encode(&self) -> Result<Vec<u8>, SettingsParseError> {
        self.settings.validate()?;
        if self.unknown.len() > MAX_UNKNOWN_SETTINGS {
            return Err(SettingsParseError::new(0, TOO_MANY_UNKNOWN));
        }

        for (index, setting) in self.unknown.iter().enumerate() {
            validate_key(&setting.key, 0)?;
            validate_value(&setting.value, 0)?;
            if KNOWN_KEYS.contains(&setting.key.as_str())
                || self.unknown[..index]
                    .iter()
                    .any(|existing| existing.key == setting.key)
            {
                return Err(SettingsParseError::new(0, DUPLICATE_KEY));
            }
        }

        let mut output = String::from("# CursorPeek settings\n");
        use fmt::Write as _;
        writeln!(output, "dwell_delay_ms={}", self.settings.dwell_delay_ms)
            .expect("writing to a String cannot fail");
        writeln!(output, "preview_width={}", self.settings.preview_width)
            .expect("writing to a String cannot fail");
        writeln!(output, "preview_height={}", self.settings.preview_height)
            .expect("writing to a String cannot fail");
        writeln!(output, "cache_entries={}", self.settings.cache_entries)
            .expect("writing to a String cannot fail");
        writeln!(output, "theme={}", self.settings.theme.as_str())
            .expect("writing to a String cannot fail");
        writeln!(
            output,
            "legacy_encoding={}",
            self.settings.legacy_encoding.as_str()
        )
        .expect("writing to a String cannot fail");
        writeln!(
            output,
            "start_with_windows={}",
            self.settings.start_with_windows
        )
        .expect("writing to a String cannot fail");
        writeln!(output, "video_previews={}", self.settings.video_previews)
            .expect("writing to a String cannot fail");
        writeln!(output, "video_audio={}", self.settings.video_audio)
            .expect("writing to a String cannot fail");
        if !self.unknown.is_empty() {
            output.push_str("\n# Preserved settings not understood by this version\n");
            for setting in &self.unknown {
                writeln!(output, "{}={}", setting.key, setting.value)
                    .expect("writing to a String cannot fail");
            }
        }

        if output.len() > MAX_CONFIG_BYTES {
            return Err(SettingsParseError::new(0, ENCODED_CONFIG_TOO_LARGE));
        }
        Ok(output.into_bytes())
    }
}

const KNOWN_KEYS: &[&str] = &[
    "dwell_delay_ms",
    "preview_width",
    "preview_height",
    "cache_entries",
    "theme",
    "legacy_encoding",
    "start_with_windows",
    "video_previews",
    "video_audio",
    "video_smooth_start",
];

const LINE_TOO_LONG: &str = "line exceeds 1024 bytes";
const DUPLICATE_KEY: &str = "duplicate key";
const TOO_MANY_UNKNOWN: &str = "more than 64 unknown settings are not allowed";
const ENCODED_CONFIG_TOO_LARGE: &str = "encoded configuration exceeds 32768 bytes";
const INVALID_UNSIGNED_INTEGER: &str = "expected an unsigned decimal integer";
const INVALID_DWELL_DELAY: &str = "invalid `dwell_delay_ms` value: expected 50-2000";
const INVALID_PREVIEW_WIDTH: &str = "invalid `preview_width` value: expected 320-960";
const INVALID_PREVIEW_HEIGHT: &str = "invalid `preview_height` value: expected 240-720";
const INVALID_CACHE_ENTRIES: &str = "invalid `cache_entries` value: expected 0-512";
const INVALID_THEME: &str = "invalid `theme` value: expected `system`, `light`, or `dark`";
const INVALID_LEGACY_ENCODING: &str = "invalid `legacy_encoding` value: expected `auto`, `system`, `off`, or a supported legacy encoding label";
const INVALID_START_WITH_WINDOWS: &str =
    "invalid `start_with_windows` value: expected `true` or `false`";
const INVALID_VIDEO_PREVIEWS: &str = "invalid `video_previews` value: expected `true` or `false`";
const INVALID_VIDEO_AUDIO: &str = "invalid `video_audio` value: expected `true` or `false`";
const INVALID_VIDEO_SMOOTH_START: &str =
    "invalid `video_smooth_start` value: expected `true` or `false`";

fn parse_boolean(
    value: &str,
    line_number: usize,
    message: &'static str,
) -> Result<bool, SettingsParseError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(SettingsParseError::new(line_number, message)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsFile {
    mode: SettingsMode,
    path: PathBuf,
}

impl SettingsFile {
    pub(crate) fn discover() -> Result<Self, SettingsError> {
        let executable = env::current_exe()
            .map_err(|source| SettingsError::io("locate the executable", PathBuf::new(), source))?;
        let directory = executable_directory(&executable)?;
        match portable_marker_state(directory)? {
            true => Ok(Self {
                mode: SettingsMode::Portable,
                path: directory.join(CONFIG_FILE_NAME),
            }),
            false => {
                let local_app_data = current_local_app_data()?;
                Ok(Self {
                    mode: SettingsMode::Installed,
                    path: local_app_data
                        .join(APPLICATION_DIRECTORY)
                        .join(CONFIG_FILE_NAME),
                })
            }
        }
    }

    #[cfg(test)]
    fn from_roots(executable: &Path, local_app_data: &Path) -> Result<Self, SettingsError> {
        let directory = executable_directory(executable)?;
        if portable_marker_state(directory)? {
            Ok(Self {
                mode: SettingsMode::Portable,
                path: directory.join(CONFIG_FILE_NAME),
            })
        } else {
            Ok(Self {
                mode: SettingsMode::Installed,
                path: local_app_data
                    .join(APPLICATION_DIRECTORY)
                    .join(CONFIG_FILE_NAME),
            })
        }
    }

    pub(crate) fn load_or_create(&self) -> Result<SettingsDocument, SettingsError> {
        if let Some(document) = self.load()? {
            return Ok(document);
        }

        let document = SettingsDocument::default();
        self.save(&document)?;
        Ok(document)
    }

    fn load(&self) -> Result<Option<SettingsDocument>, SettingsError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(SettingsError::io(
                    "open the configuration",
                    self.path.clone(),
                    source,
                ));
            }
        };

        let mut bytes = Vec::with_capacity(1_024);
        Read::by_ref(&mut file)
            .take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| {
                SettingsError::io("read the configuration", self.path.clone(), source)
            })?;
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(SettingsError::TooLarge {
                path: self.path.clone(),
                limit: MAX_CONFIG_BYTES,
            });
        }

        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        let text = std::str::from_utf8(bytes).map_err(|source| SettingsError::InvalidUtf8 {
            path: self.path.clone(),
            source,
        })?;
        SettingsDocument::parse(text).map(Some).map_err(|source| {
            SettingsError::InvalidConfiguration {
                path: self.path.clone(),
                source,
            }
        })
    }

    pub(crate) fn save(&self, document: &SettingsDocument) -> Result<(), SettingsError> {
        let bytes = document
            .encode()
            .map_err(|source| SettingsError::InvalidConfiguration {
                path: self.path.clone(),
                source,
            })?;
        let directory = self.path.parent().ok_or_else(|| {
            SettingsError::InvalidPath(format!(
                "configuration path `{}` has no directory",
                self.path.display()
            ))
        })?;
        fs::create_dir_all(directory).map_err(|source| {
            SettingsError::io(
                "create the configuration directory",
                directory.to_owned(),
                source,
            )
        })?;

        let (mut file, mut temporary) = create_temporary_file(directory)?;
        file.write_all(&bytes).map_err(|source| {
            SettingsError::io(
                "write the temporary configuration",
                temporary.path().to_owned(),
                source,
            )
        })?;
        file.sync_all().map_err(|source| {
            SettingsError::io(
                "flush the temporary configuration",
                temporary.path().to_owned(),
                source,
            )
        })?;
        drop(file);

        move_file_into_place(temporary.path(), &self.path)?;
        temporary.disarm();
        Ok(())
    }

    pub(crate) const fn mode(&self) -> SettingsMode {
        self.mode
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }
}

fn executable_directory(executable: &Path) -> Result<&Path, SettingsError> {
    executable
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .ok_or_else(|| {
            SettingsError::InvalidPath(format!(
                "executable path `{}` has no directory",
                executable.display()
            ))
        })
}

fn portable_marker_state(directory: &Path) -> Result<bool, SettingsError> {
    let marker = directory.join(PORTABLE_MARKER_NAME);
    match fs::metadata(&marker) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(SettingsError::InvalidPath(format!(
            "portable marker `{}` is not a file",
            marker.display()
        ))),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(SettingsError::io(
            "inspect the portable marker",
            marker,
            source,
        )),
    }
}

fn current_local_app_data() -> Result<PathBuf, SettingsError> {
    // SAFETY: the known-folder identifier and zero flags are valid, and a null token selects the
    // current user. The returned task allocation is immediately placed under one Drop owner.
    let path =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG::default(), None) }
            .map_err(|source| SettingsError::Windows {
                operation: "locate the current user's Local AppData folder",
                source,
            })?;
    let path = OwnedTaskPath(path);
    // SAFETY: SHGetKnownFolderPath promises a null-terminated task-allocated UTF-16 string. The
    // owner remains live while this bounded copy is made.
    let units = unsafe { path.units() }.ok_or(SettingsError::InvalidKnownFolderPath)?;
    Ok(PathBuf::from(OsString::from_wide(units)))
}

fn create_temporary_file(directory: &Path) -> Result<(File, TemporaryPath), SettingsError> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".{CONFIG_FILE_NAME}.tmp.{}.{}",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, TemporaryPath(Some(path)))),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(SettingsError::io(
                    "create the temporary configuration",
                    path,
                    source,
                ));
            }
        }
    }

    Err(SettingsError::TemporaryNameExhausted(directory.to_owned()))
}

fn move_file_into_place(source: &Path, destination: &Path) -> Result<(), SettingsError> {
    let source_wide = null_terminated_path(source)?;
    let destination_wide = null_terminated_path(destination)?;
    // SAFETY: both buffers are live null-terminated UTF-16 paths. The temporary file and
    // destination share a directory, so this cannot degrade into a cross-volume copy.
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|source| SettingsError::Windows {
        operation: "publish the configuration",
        source,
    })
}

fn null_terminated_path(path: &Path) -> Result<Vec<u16>, SettingsError> {
    let mut units = Vec::with_capacity(path.as_os_str().len() + 1);
    for unit in path.as_os_str().encode_wide() {
        if unit == 0 {
            return Err(SettingsError::InvalidPath(format!(
                "path `{}` contains a null character",
                path.display()
            )));
        }
        units.push(unit);
    }
    units.push(0);
    Ok(units)
}

fn validate_key(key: &str, line: usize) -> Result<(), SettingsParseError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(SettingsParseError::new(
            line,
            "key length must be 1-64 ASCII bytes",
        ));
    }
    let mut bytes = key.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(SettingsParseError::new(line, "invalid key"));
    }
    Ok(())
}

fn validate_value(value: &str, line: usize) -> Result<(), SettingsParseError> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(SettingsParseError::new(line, "value exceeds 1024 bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(SettingsParseError::new(
            line,
            "values cannot contain control characters",
        ));
    }
    Ok(())
}

fn parse_bounded_integer(
    value: &str,
    minimum: u64,
    maximum: u64,
    line: usize,
    range_error: &'static str,
) -> Result<u64, SettingsParseError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SettingsParseError::new(line, INVALID_UNSIGNED_INTEGER));
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| SettingsParseError::new(line, INVALID_UNSIGNED_INTEGER))?;
    if (minimum..=maximum).contains(&parsed) {
        Ok(parsed)
    } else {
        Err(SettingsParseError::new(line, range_error))
    }
}

struct OwnedTaskPath(PWSTR);

impl OwnedTaskPath {
    unsafe fn units(&self) -> Option<&[u16]> {
        if self.0.0.is_null() {
            return None;
        }
        for length in 0..=MAX_KNOWN_FOLDER_UNITS {
            // SAFETY: the Known Folder API promises a valid null-terminated task allocation. The
            // scan is capped at the documented extended Windows path scale.
            if unsafe { *self.0.0.add(length) } == 0 {
                // SAFETY: the bounded scan proved these initialized units precede the terminator.
                return Some(unsafe { std::slice::from_raw_parts(self.0.0, length) });
            }
        }
        None
    }
}

impl Drop for OwnedTaskPath {
    fn drop(&mut self) {
        // SAFETY: SHGetKnownFolderPath transfers one CoTaskMemAlloc-compatible pointer. This owner
        // frees it exactly once, including when the bounded conversion rejects the result.
        unsafe { CoTaskMemFree(Some(self.0.0.cast())) }
    }
}

struct TemporaryPath(Option<PathBuf>);

impl TemporaryPath {
    fn path(&self) -> &Path {
        self.0
            .as_deref()
            .expect("an armed temporary path always contains its path")
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[derive(Debug)]
pub(crate) struct SettingsParseError {
    line: usize,
    reason: &'static str,
}

impl SettingsParseError {
    const fn new(line: usize, reason: &'static str) -> Self {
        Self { line, reason }
    }
}

impl fmt::Display for SettingsParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            formatter.write_str(self.reason)
        } else {
            write!(formatter, "line {}: {}", self.line, self.reason)
        }
    }
}

impl Error for SettingsParseError {}

#[derive(Debug)]
pub(crate) enum SettingsError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Windows {
        operation: &'static str,
        source: WindowsError,
    },
    InvalidConfiguration {
        path: PathBuf,
        source: SettingsParseError,
    },
    InvalidUtf8 {
        path: PathBuf,
        source: std::str::Utf8Error,
    },
    TooLarge {
        path: PathBuf,
        limit: usize,
    },
    UnsupportedMode {
        operation: &'static str,
        mode: SettingsMode,
    },
    InvalidPath(String),
    InvalidKnownFolderPath,
    TemporaryNameExhausted(PathBuf),
}

impl SettingsError {
    fn io(operation: &'static str, path: PathBuf, source: io::Error) -> Self {
        Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } if path.as_os_str().is_empty() => write!(formatter, "{operation}: {source}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} `{}`: {source}", path.display()),
            Self::Windows { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::InvalidConfiguration { path, source } => {
                write!(
                    formatter,
                    "invalid configuration `{}`: {source}",
                    path.display()
                )
            }
            Self::InvalidUtf8 { path, .. } => {
                write!(
                    formatter,
                    "configuration `{}` is not valid UTF-8",
                    path.display()
                )
            }
            Self::TooLarge { path, limit } => write!(
                formatter,
                "configuration `{}` exceeds {limit} bytes",
                path.display()
            ),
            Self::UnsupportedMode { operation, mode } => {
                write!(
                    formatter,
                    "{operation} is not supported in {} mode",
                    mode.as_str()
                )
            }
            Self::InvalidPath(reason) => formatter.write_str(reason),
            Self::InvalidKnownFolderPath => {
                formatter.write_str("Local AppData returned an invalid path")
            }
            Self::TemporaryNameExhausted(directory) => write!(
                formatter,
                "could not reserve a temporary configuration name in `{}`",
                directory.display()
            ),
        }
    }
}

impl Error for SettingsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Windows { source, .. } => Some(source),
            Self::InvalidConfiguration { source, .. } => Some(source),
            Self::InvalidUtf8 { source, .. } => Some(source),
            Self::TooLarge { .. }
            | Self::UnsupportedMode { .. }
            | Self::InvalidPath(_)
            | Self::InvalidKnownFolderPath
            | Self::TemporaryNameExhausted(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APPLICATION_DIRECTORY, CONFIG_FILE_NAME, LegacyEncoding, MAX_CONFIG_BYTES,
        PORTABLE_MARKER_NAME, SettingsDocument, SettingsError, SettingsFile, SettingsMode, Theme,
        current_local_app_data,
    };
    use std::{
        env, fs, io,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn defaults_are_canonical_and_round_trip() {
        let document = SettingsDocument::default();
        let bytes = document.encode().expect("defaults should encode");
        let text = std::str::from_utf8(&bytes).expect("settings are UTF-8");

        assert_eq!(
            text,
            "# CursorPeek settings\n\
             dwell_delay_ms=50\n\
             preview_width=640\n\
             preview_height=480\n\
             cache_entries=128\n\
             theme=system\n\
             legacy_encoding=auto\n\
             start_with_windows=false\n\
             video_previews=true\n\
             video_audio=false\n"
        );
        assert_eq!(
            SettingsDocument::parse(text).expect("canonical defaults should parse"),
            document
        );
        assert_eq!(document.settings().dwell_delay(), Duration::from_millis(50));
    }

    #[test]
    fn valid_values_and_unknown_settings_survive_canonical_save() {
        let input = "\u{feff}; user edit\n\
             dwell_delay_ms = 50\n\
             preview_width=960\n\
             preview_height=720\n\
             cache_entries=512\n\
             theme=dark\n\
                     legacy_encoding=Windows-1252\n\
	                     start_with_windows=true\n\
	                     video_previews=false\n\
	                     video_audio=true\n\
	                     video_smooth_start=false\n\
                     future.setting = 保留\n";
        let document = SettingsDocument::parse(input).expect("valid settings should parse");

        assert_eq!(document.settings.dwell_delay_ms, 50);
        assert_eq!(document.settings.preview_width, 960);
        assert_eq!(document.settings.preview_height, 720);
        assert_eq!(document.settings.cache_entries, 512);
        assert_eq!(document.settings.theme, Theme::Dark);
        assert_eq!(
            document.settings.legacy_encoding,
            LegacyEncoding::Label("windows-1252".to_owned())
        );
        assert!(document.settings.start_with_windows);
        assert!(!document.settings.video_previews);
        assert!(document.settings.video_audio);

        let encoded = document.encode().expect("valid settings should encode");
        let encoded = std::str::from_utf8(&encoded).expect("settings are UTF-8");
        assert!(encoded.contains("legacy_encoding=windows-1252\n"));
        assert!(!encoded.contains("video_smooth_start"));
        assert!(encoded.contains("future.setting=保留\n"));
        assert_eq!(
            SettingsDocument::parse(encoded).expect("saved settings should parse"),
            document
        );
        assert_eq!(
            LegacyEncoding::parse("latin1"),
            Some(LegacyEncoding::Label("windows-1252".to_owned()))
        );
    }

    #[test]
    fn tray_mutations_validate_known_values_and_preserve_unknown_settings() {
        let mut document = SettingsDocument::parse(
            "dwell_delay_ms=400\npreview_width=640\npreview_height=480\nfuture=keep\n",
        )
        .expect("the starting document should parse");

        document
            .set_dwell_delay_ms(700)
            .expect("the tray dwell preset should be valid");
        document
            .set_preview_size(800, 600)
            .expect("the tray size preset should be valid");
        document.set_start_with_windows(true);
        document.set_theme(Theme::Dark);
        document.set_video_previews(false);
        document.set_video_audio(true);
        assert_eq!(document.settings().dwell_delay_ms(), 700);
        assert_eq!(
            (
                document.settings().preview_width(),
                document.settings().preview_height()
            ),
            (800, 600)
        );
        assert!(document.settings().start_with_windows());
        assert_eq!(document.settings().theme(), Theme::Dark);
        assert!(!document.settings().video_previews());
        assert!(document.settings().video_audio());
        assert!(
            std::str::from_utf8(&document.encode().unwrap())
                .unwrap()
                .contains("future=keep")
        );

        assert!(document.set_dwell_delay_ms(49).is_err());
        assert!(document.set_preview_size(319, 240).is_err());
        assert_eq!(document.settings().dwell_delay_ms(), 700);
        assert_eq!(document.settings().preview_width(), 800);
    }

    #[test]
    fn malformed_duplicate_and_invalid_known_values_fail_closed() {
        for (input, expected) in [
            ("dwell_delay_ms=49\n", "expected 50-2000"),
            ("dwell_delay_ms=+400\n", "unsigned decimal"),
            ("preview_width=961\n", "expected 320-960"),
            ("preview_height=239\n", "expected 240-720"),
            ("cache_entries=513\n", "expected 0-512"),
            ("cache_entries=-1\n", "unsigned decimal"),
            ("theme=automatic\n", "system"),
            ("legacy_encoding=utf 8\n", "supported legacy"),
            ("legacy_encoding=utf-8\n", "supported legacy"),
            ("legacy_encoding=x-user-defined\n", "supported legacy"),
            ("legacy_encoding=not-a-codepage\n", "supported legacy"),
            ("start_with_windows=1\n", "true"),
            ("video_previews=1\n", "true"),
            ("video_audio=yes\n", "true"),
            ("video_smooth_start=fast\n", "true"),
            ("theme=dark\ntheme=light\n", "duplicate"),
            ("missing separator\n", "key=value"),
            ("9invalid=value\n", "invalid key"),
        ] {
            let error = SettingsDocument::parse(input).expect_err(input);
            assert!(
                error.to_string().contains(expected),
                "`{input}` returned `{error}`, expected `{expected}`"
            );
        }
    }

    #[test]
    fn installed_and_portable_paths_are_deterministic() {
        let root = TestDirectory::new("paths");
        let executable_directory = root.path().join("app");
        fs::create_dir(&executable_directory).expect("app directory should be created");
        let executable = executable_directory.join("CursorPeek.exe");
        fs::write(&executable, []).expect("placeholder executable should be created");
        let local_app_data = root.path().join("local");

        let installed = SettingsFile::from_roots(&executable, &local_app_data)
            .expect("missing marker should select installed mode");
        assert_eq!(installed.mode(), SettingsMode::Installed);
        assert_eq!(
            installed.path(),
            local_app_data
                .join(APPLICATION_DIRECTORY)
                .join(CONFIG_FILE_NAME)
        );

        fs::write(
            executable_directory.join(PORTABLE_MARKER_NAME),
            b"portable\n",
        )
        .expect("portable marker should be created");
        let portable = SettingsFile::from_roots(&executable, &local_app_data)
            .expect("marker should select portable mode");
        assert_eq!(portable.mode(), SettingsMode::Portable);
        assert_eq!(portable.path(), executable_directory.join(CONFIG_FILE_NAME));
    }

    #[test]
    fn a_non_file_portable_marker_is_rejected() {
        let root = TestDirectory::new("bad-marker");
        let executable_directory = root.path().join("app");
        fs::create_dir(&executable_directory).expect("app directory should be created");
        let executable = executable_directory.join("CursorPeek.exe");
        fs::write(&executable, []).expect("placeholder executable should be created");
        fs::create_dir(executable_directory.join(PORTABLE_MARKER_NAME))
            .expect("invalid marker directory should be created");

        let error = SettingsFile::from_roots(&executable, &root.path().join("local"))
            .expect_err("a marker directory must not enable portable mode");
        assert!(error.to_string().contains("is not a file"));
    }

    #[test]
    fn missing_file_is_created_and_existing_settings_are_replaced_atomically() {
        let root = TestDirectory::new("atomic");
        let file = test_settings_file(&root, "配置");
        let defaults = file
            .load_or_create()
            .expect("missing configuration should create defaults");
        assert_eq!(defaults, SettingsDocument::default());

        let mut replacement = defaults;
        replacement.settings.dwell_delay_ms = 2_000;
        replacement.unknown.push(super::UnknownSetting {
            key: "future".to_owned(),
            value: "保留".to_owned(),
        });
        file.save(&replacement)
            .expect("the existing configuration should be replaced");
        assert_eq!(
            file.load()
                .expect("replacement should load")
                .expect("replacement should exist"),
            replacement
        );
        assert_no_temporary_files(file.path().parent().expect("config has a parent"));
    }

    #[test]
    fn invalid_existing_file_is_never_overwritten() {
        let root = TestDirectory::new("invalid");
        let file = test_settings_file(&root, "local");
        let path = file.path();
        fs::create_dir_all(path.parent().expect("config has a parent"))
            .expect("config directory should be created");
        let invalid = b"dwell_delay_ms=49\n";
        fs::write(path, invalid).expect("invalid fixture should be written");

        let error = file
            .load_or_create()
            .expect_err("invalid settings must be rejected");
        assert!(matches!(error, SettingsError::InvalidConfiguration { .. }));
        assert_eq!(
            fs::read(path).expect("fixture should remain readable"),
            invalid
        );
        assert_no_temporary_files(path.parent().expect("config has a parent"));
    }

    #[test]
    fn publication_failure_keeps_the_previous_destination_and_cleans_the_temp_file() {
        let root = TestDirectory::new("publish-failure");
        let destination = root.path().join(CONFIG_FILE_NAME);
        fs::create_dir(&destination).expect("destination directory should be created");
        let file = SettingsFile {
            mode: SettingsMode::Portable,
            path: destination.clone(),
        };

        file.save(&SettingsDocument::default())
            .expect_err("a directory cannot be replaced by the config file");
        assert!(destination.is_dir());
        assert_no_temporary_files(root.path());
    }

    #[test]
    fn file_size_and_utf8_are_bounded_before_parsing() {
        let root = TestDirectory::new("bounds");
        let file = test_settings_file(&root, "local");
        let path = file.path();
        fs::create_dir_all(path.parent().expect("config has a parent"))
            .expect("config directory should be created");

        fs::write(path, vec![b'x'; MAX_CONFIG_BYTES + 1])
            .expect("oversized fixture should be written");
        assert!(matches!(
            file.load().expect_err("oversized file must fail"),
            SettingsError::TooLarge { .. }
        ));

        fs::write(path, [0xff, 0xfe]).expect("invalid UTF-8 fixture should be written");
        assert!(matches!(
            file.load().expect_err("invalid UTF-8 must fail"),
            SettingsError::InvalidUtf8 { .. }
        ));
    }

    #[test]
    fn local_app_data_comes_from_the_current_user_known_folder() {
        let path = current_local_app_data().expect("Local AppData should resolve");
        assert!(path.is_absolute());
        assert!(!path.as_os_str().is_empty());
    }

    fn test_settings_file(root: &TestDirectory, local_name: &str) -> SettingsFile {
        let executable_directory = root.path().join("app");
        fs::create_dir(&executable_directory).expect("app directory should be created");
        let executable = executable_directory.join("CursorPeek.exe");
        fs::write(&executable, []).expect("placeholder executable should be created");
        SettingsFile::from_roots(&executable, &root.path().join(local_name))
            .expect("test settings path should resolve")
    }

    fn assert_no_temporary_files(directory: &Path) {
        let names = fs::read_dir(directory)
            .expect("configuration directory should be readable")
            .map(|entry| {
                entry
                    .expect("directory entry should be readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(&format!(".{CONFIG_FILE_NAME}.tmp.")))
            .collect::<Vec<_>>();
        assert!(names.is_empty(), "temporary files remain: {names:?}");
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            for _ in 0..32 {
                let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = env::temp_dir().join(format!(
                    "cursorpeek-settings-{label}-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("test directory `{}` failed: {error}", path.display()),
                }
            }
            panic!("could not reserve a unique settings test directory");
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
