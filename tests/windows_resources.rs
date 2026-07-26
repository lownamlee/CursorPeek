use std::{
    ffi::{OsStr, c_void},
    os::windows::ffi::OsStrExt,
    path::Path,
    process::Command,
    ptr,
};

use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::LibraryLoader::{
            FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW,
        },
        UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_MANIFEST, RT_VERSION},
    },
    core::PCWSTR,
};

const APPLICATION_ICON_RESOURCE_ID: u16 = 101;
const EXPECTED_ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[test]
fn gui_executable_keeps_redirected_version_output() {
    let output = Command::new(env!("CARGO_BIN_EXE_CursorPeek"))
        .arg("--version")
        .output()
        .expect("the GUI executable should start with redirected handles");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).expect("version output should be UTF-8"),
        format!("CursorPeek {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn executable_uses_gui_subsystem_and_contains_required_resources() {
    let executable = Path::new(env!("CARGO_BIN_EXE_CursorPeek"));
    let image =
        std::fs::read(executable).expect("the built CursorPeek executable should be readable");
    let optional_header = pe_optional_header(&image);

    assert_eq!(read_u16(&image, optional_header + 68), 2, "PE subsystem");
    assert_ne!(
        read_u32(&image, optional_header + 128),
        0,
        "resource-table RVA"
    );
    assert_ne!(
        read_u32(&image, optional_header + 132),
        0,
        "resource-table size"
    );

    let module = ResourceModule::load(executable);
    assert!(module.contains(1, RT_MANIFEST), "manifest resource");
    assert!(
        module.contains(APPLICATION_ICON_RESOURCE_ID, RT_GROUP_ICON),
        "application icon group"
    );
    assert!(module.contains(1, RT_VERSION), "version resource");
}

#[test]
fn executable_version_strings_match_the_cargo_package() {
    let executable = Path::new(env!("CARGO_BIN_EXE_CursorPeek"));
    let version = FileVersion::read(executable);

    assert_eq!(version.query("ProductName"), "CursorPeek");
    assert_eq!(version.query("InternalName"), "CursorPeek");
    assert_eq!(version.query("OriginalFilename"), "CursorPeek.exe");
    assert_eq!(
        version.query("FileDescription"),
        "File Explorer hover previews"
    );
    assert_eq!(version.query("CompanyName"), "CursorPeek contributors");
    assert_eq!(version.query("ProductVersion"), env!("CARGO_PKG_VERSION"));
    assert_eq!(
        version.query("FileVersion"),
        format!("{}.0", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn checked_in_icon_contains_every_required_rgba_png_size() {
    let icon = include_bytes!("../assets/windows/CursorPeek.ico");
    assert_eq!(read_u16(icon, 0), 0, "ICO reserved field");
    assert_eq!(read_u16(icon, 2), 1, "ICO image type");
    assert_eq!(
        usize::from(read_u16(icon, 4)),
        EXPECTED_ICON_SIZES.len(),
        "ICO directory count"
    );

    let mut actual_sizes = Vec::new();
    for index in 0..EXPECTED_ICON_SIZES.len() {
        let entry = 6 + (index * 16);
        let width = icon_dimension(icon[entry]);
        let height = icon_dimension(icon[entry + 1]);
        assert_eq!(width, height, "ICO entry {index} must be square");
        assert_eq!(icon[entry + 2], 0, "ICO color count");
        assert_eq!(icon[entry + 3], 0, "ICO reserved byte");
        assert_eq!(read_u16(icon, entry + 4), 1, "ICO color planes");
        assert_eq!(read_u16(icon, entry + 6), 32, "ICO bit depth");

        let payload_length = read_u32(icon, entry + 8) as usize;
        let payload_offset = read_u32(icon, entry + 12) as usize;
        let payload_end = payload_offset
            .checked_add(payload_length)
            .expect("ICO payload range should not overflow");
        let payload = icon
            .get(payload_offset..payload_end)
            .expect("ICO payload should remain inside the asset");

        assert_eq!(&payload[..PNG_SIGNATURE.len()], PNG_SIGNATURE);
        assert_eq!(&payload[12..16], b"IHDR");
        assert_eq!(
            u32::from_be_bytes(payload[16..20].try_into().unwrap()),
            width
        );
        assert_eq!(
            u32::from_be_bytes(payload[20..24].try_into().unwrap()),
            height
        );
        assert_eq!(payload[24], 8, "PNG bit depth");
        assert_eq!(payload[25], 6, "PNG color type must be RGBA");
        actual_sizes.push(width);
    }

    assert_eq!(actual_sizes, EXPECTED_ICON_SIZES);
}

fn pe_optional_header(image: &[u8]) -> usize {
    assert_eq!(&image[..2], b"MZ", "DOS signature");
    let pe = read_u32(image, 0x3c) as usize;
    assert_eq!(&image[pe..pe + 4], b"PE\0\0", "PE signature");
    let optional_header = pe + 24;
    assert_eq!(read_u16(image, optional_header), 0x20b, "PE32+ magic");
    optional_header
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("two-byte field should be present"),
    )
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("four-byte field should be present"),
    )
}

fn icon_dimension(encoded: u8) -> u32 {
    if encoded == 0 {
        256
    } else {
        u32::from(encoded)
    }
}

fn wide_z(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn integer_resource(id: u16) -> PCWSTR {
    PCWSTR(id as usize as *const u16)
}

struct ResourceModule(HMODULE);

impl ResourceModule {
    fn load(path: &Path) -> Self {
        let path = wide_z(path.as_os_str());
        // SAFETY: The path is terminated and remains alive for the call. Data-file/image-resource
        // loading does not run executable initialization, and Drop releases the returned module.
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(path.as_ptr()),
                None,
                LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
            )
        }
        .expect("CursorPeek should load as an image resource");
        Self(module)
    }

    fn contains(&self, id: u16, resource_type: PCWSTR) -> bool {
        // SAFETY: The module remains loaded, both identifiers use documented integer-resource
        // encoding, and FindResourceW only searches the module's immutable resource directory.
        !unsafe { FindResourceW(Some(self.0), integer_resource(id), resource_type) }.is_invalid()
    }
}

impl Drop for ResourceModule {
    fn drop(&mut self) {
        // SAFETY: This object owns the successful LoadLibraryExW result and releases it once.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

struct FileVersion(Vec<u8>);

impl FileVersion {
    fn read(path: &Path) -> Self {
        let path = wide_z(path.as_os_str());
        // SAFETY: The terminated path remains alive for both calls. The allocated buffer has the
        // exact size returned by Windows and is passed as writable storage.
        unsafe {
            let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
            assert_ne!(size, 0, "version-resource size");
            let mut data = vec![0_u8; size as usize];
            GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr().cast())
                .expect("CursorPeek version information should load");
            Self(data)
        }
    }

    fn query(&self, name: &str) -> String {
        let query = format!(r"\StringFileInfo\040904B0\{name}");
        let query = wide_z(OsStr::new(&query));
        let mut value: *mut c_void = ptr::null_mut();
        let mut length = 0_u32;

        // SAFETY: The version block and terminated query remain alive. Windows returns a borrowed
        // UTF-16 pointer and length into that immutable block, used only before this method returns.
        let found = unsafe {
            VerQueryValueW(
                self.0.as_ptr().cast(),
                PCWSTR(query.as_ptr()),
                &mut value,
                &mut length,
            )
        };
        assert!(found.as_bool(), "missing version string {name}");
        assert!(!value.is_null(), "null version string {name}");
        assert_ne!(length, 0, "empty version string {name}");

        // SAFETY: VerQueryValueW returned `length` UTF-16 code units inside the retained block.
        let units = unsafe { std::slice::from_raw_parts(value.cast::<u16>(), length as usize) };
        let units = units.strip_suffix(&[0]).unwrap_or(units);
        String::from_utf16(units).expect("version strings should be valid UTF-16")
    }
}
