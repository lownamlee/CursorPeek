use std::ffi::c_void;

use crate::hover::PhysicalScreenPoint;
use cursorpeek_core::ExplorerWindowId;

use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, POINT},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{
            GA_ROOT, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
            IsIconic, IsWindow, IsWindowVisible, WindowFromPhysicalPoint,
        },
    },
    core::{Owned, PWSTR},
};

const CLASS_NAME_CAPACITY: usize = 64;
const PROCESS_IMAGE_CAPACITY: usize = 512;
const CABINET_WINDOW_CLASS: &[u8] = b"CabinetWClass";
const EXPLORE_WINDOW_CLASS: &[u8] = b"ExploreWClass";
const EXPLORER_IMAGE_NAME: &[u8] = b"explorer.exe";

pub(super) fn explorer_window_at(point: PhysicalScreenPoint) -> Option<ExplorerWindowId> {
    explorer_window_id(root_window(window_at(point))?)
}

pub(super) fn point_belongs_to_explorer_window(
    point: PhysicalScreenPoint,
    expected: ExplorerWindowId,
) -> bool {
    root_window(window_at(point)).and_then(window_id) == Some(expected)
}

pub(super) fn explorer_window_is_available(expected: ExplorerWindowId) -> bool {
    window_from_id(expected).is_some_and(explorer_root_is_available)
}

pub(super) fn is_foreground_explorer_window_at(point: PhysicalScreenPoint) -> bool {
    let Some(point_root) = root_window(window_at(point)) else {
        return false;
    };

    // SAFETY: This has no pointer or ownership requirements. The returned HWND is borrowed and
    // may be null while activation is changing, which fails this diagnostic eligibility check.
    let foreground = unsafe { GetForegroundWindow() };
    let Some(foreground_root) = root_window(foreground) else {
        return false;
    };

    point_root == foreground_root && explorer_root_is_available(point_root)
}

#[cfg(test)]
pub(super) fn is_explorer_window(window: HWND) -> bool {
    root_window(window).is_some_and(explorer_root_is_available)
}

pub(super) fn belongs_to_explorer_window(window: HWND, expected: ExplorerWindowId) -> bool {
    root_window(window).and_then(window_id) == Some(expected)
}

fn window_at(point: PhysicalScreenPoint) -> HWND {
    // SAFETY: `point` contains physical screen coordinates sampled by GetPhysicalCursorPos.
    // The returned HWND is borrowed and used only for synchronous queries.
    unsafe {
        WindowFromPhysicalPoint(POINT {
            x: point.x,
            y: point.y,
        })
    }
}

fn root_window(window: HWND) -> Option<HWND> {
    if window.0.is_null() {
        return None;
    }

    // SAFETY: `window` is a borrowed HWND supplied by Windows. GA_ROOT performs a synchronous
    // parent-chain lookup and returns another borrowed HWND or null on failure.
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    (!root.0.is_null()).then_some(root)
}

fn explorer_window_id(root: HWND) -> Option<ExplorerWindowId> {
    explorer_root_is_available(root)
        .then(|| window_id(root))
        .flatten()
}

fn window_id(window: HWND) -> Option<ExplorerWindowId> {
    ExplorerWindowId::try_from_raw(window.0 as usize as u64)
}

fn window_from_id(id: ExplorerWindowId) -> Option<HWND> {
    let raw = usize::try_from(id.get()).ok()?;
    Some(HWND(raw as *mut c_void))
}

fn explorer_root_is_available(root: HWND) -> bool {
    // SAFETY: `root` is a copied HWND value. Each query is synchronous and returns false for a
    // destroyed or otherwise invalid window. A minimized or hidden Explorer cannot own a hover
    // target at a physical screen point.
    unsafe {
        IsWindow(Some(root)).as_bool()
            && IsWindowVisible(root).as_bool()
            && !IsIconic(root).as_bool()
            && root_window(root) == Some(root)
            && is_explorer_root(root)
    }
}

fn is_explorer_root(root: HWND) -> bool {
    if !has_explorer_frame_class(root) {
        return false;
    }

    explorer_process_id(root).is_some_and(process_image_is_explorer)
}

fn has_explorer_frame_class(window: HWND) -> bool {
    let mut class_name = [0_u16; CLASS_NAME_CAPACITY];

    // SAFETY: `class_name` is live writable storage. The HWND is borrowed for this synchronous
    // query, and Windows returns the copied character count without the terminating null.
    let length = unsafe { GetClassNameW(window, &mut class_name) };
    if length <= 0 || length as usize >= class_name.len() - 1 {
        return false;
    }

    let class_name = &class_name[..length as usize];
    wide_ascii_eq_ignore_case(class_name, CABINET_WINDOW_CLASS)
        || wide_ascii_eq_ignore_case(class_name, EXPLORE_WINDOW_CLASS)
}

fn explorer_process_id(window: HWND) -> Option<u32> {
    let mut process_id = 0_u32;

    // SAFETY: `process_id` is valid writable storage. The borrowed HWND remains live for this
    // synchronous query. A zero thread ID or process ID is treated as failure.
    let thread_id = unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    (thread_id != 0 && process_id != 0).then_some(process_id)
}

fn process_image_is_explorer(process_id: u32) -> bool {
    // SAFETY: The process ID came from a live window. We request only documented query access,
    // make the handle non-inheritable, and transfer the returned owned handle immediately.
    let Ok(process) =
        (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) })
    else {
        return false;
    };
    // SAFETY: OpenProcess returned a new owned process handle. This guard is now its only owner.
    let process = unsafe { Owned::<HANDLE>::new(process) };
    let mut image_name = [0_u16; PROCESS_IMAGE_CAPACITY];
    let mut length = image_name.len() as u32;

    // SAFETY: `process` owns a valid query handle. The buffer and character-count pointer remain
    // live for the call. Windows writes at most the supplied capacity and updates `length`.
    let query_result = unsafe {
        QueryFullProcessImageNameW(
            *process,
            PROCESS_NAME_WIN32,
            PWSTR(image_name.as_mut_ptr()),
            &mut length,
        )
    };
    if query_result.is_err() {
        return false;
    }

    let Ok(length) = usize::try_from(length) else {
        return false;
    };
    if length == 0 || length >= image_name.len() {
        return false;
    }

    wide_path_basename_matches(&image_name[..length], EXPLORER_IMAGE_NAME)
}

fn wide_path_basename_matches(path: &[u16], expected: &[u8]) -> bool {
    let end = path
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(path.len());
    let path = &path[..end];
    let start = path
        .iter()
        .rposition(|character| *character == u16::from(b'\\') || *character == u16::from(b'/'))
        .map_or(0, |separator| separator + 1);

    wide_ascii_eq_ignore_case(&path[start..], expected)
}

fn wide_ascii_eq_ignore_case(actual: &[u16], expected: &[u8]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .copied()
            .zip(expected.iter().copied())
            .all(|(actual, expected)| {
                ascii_lowercase(actual) == u16::from(expected.to_ascii_lowercase())
            })
}

fn ascii_lowercase(character: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&character) {
        character + u16::from(b'a' - b'A')
    } else {
        character
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CABINET_WINDOW_CLASS, EXPLORE_WINDOW_CLASS, EXPLORER_IMAGE_NAME, process_image_is_explorer,
        wide_ascii_eq_ignore_case, wide_path_basename_matches,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;

    #[test]
    fn explorer_frame_classes_require_an_exact_match() {
        for class_name in ["CabinetWClass", "cabinetwclass", "ExploreWClass"] {
            let class_name = class_name.encode_utf16().collect::<Vec<_>>();
            assert!(
                wide_ascii_eq_ignore_case(&class_name, CABINET_WINDOW_CLASS)
                    || wide_ascii_eq_ignore_case(&class_name, EXPLORE_WINDOW_CLASS)
            );
        }

        for class_name in [
            "CabinetWClassChild",
            "XCabinetWClass",
            "ExplorerWClass",
            "Progman",
        ] {
            let class_name = class_name.encode_utf16().collect::<Vec<_>>();
            assert!(!wide_ascii_eq_ignore_case(
                &class_name,
                CABINET_WINDOW_CLASS
            ));
            assert!(!wide_ascii_eq_ignore_case(
                &class_name,
                EXPLORE_WINDOW_CLASS
            ));
        }
    }

    #[test]
    fn explorer_image_name_requires_an_exact_basename() {
        for path in [
            r"C:\Windows\explorer.exe",
            r"C:\WINDOWS\EXPLORER.EXE",
            "explorer.exe",
            "C:/Windows/explorer.exe",
        ] {
            let path = path.encode_utf16().collect::<Vec<_>>();
            assert!(wide_path_basename_matches(&path, EXPLORER_IMAGE_NAME));
        }

        for path in [
            r"C:\Windows\notexplorer.exe",
            r"C:\Windows\explorer.exe.backup",
            r"C:\Windows\explorer.exe\child",
            r"C:\Windows\explorer.ex",
        ] {
            let path = path.encode_utf16().collect::<Vec<_>>();
            assert!(!wide_path_basename_matches(&path, EXPLORER_IMAGE_NAME));
        }
    }

    #[test]
    fn current_process_is_not_mistaken_for_explorer() {
        // SAFETY: GetCurrentProcessId has no pointer, lifetime, or ownership requirements.
        let process_id = unsafe { GetCurrentProcessId() };
        assert!(!process_image_is_explorer(process_id));
    }
}
