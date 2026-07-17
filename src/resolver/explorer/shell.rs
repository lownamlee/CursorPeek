use std::{
    ffi::{OsString, c_void},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
};

use windows::{
    Win32::{
        Foundation::{HWND, POINT, RECT},
        System::{
            Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IServiceProvider},
            SystemServices::{SFGAO_FILESYSTEM, SFGAO_FOLDER},
            Variant::VARIANT,
        },
        UI::{
            Shell::{
                IFolderView2, IShellBrowser, IShellItem, IShellItemArray, IShellView,
                IShellWindows, IWebBrowser2, SID_STopLevelBrowser, SIGDN_FILESYSPATH,
                SVGIO_ALLVIEW, ShellWindows,
            },
            WindowsAndMessaging::{
                GA_ROOT, GetAncestor, GetForegroundWindow, GetWindowRect, IsChild, IsIconic,
                IsWindowVisible, WindowFromPoint,
            },
        },
    },
    core::{Interface, PWSTR},
};

use crate::{hover::PhysicalScreenPoint, resolver::ResolvedTarget};

use super::candidate::CandidateEvidence;

const MAX_SHELL_WINDOWS: i32 = 64;
const MAX_VIEW_ITEMS: u32 = 10_000;
const MAX_FILESYSTEM_PATH_UNITS: usize = 32_767;

pub(super) struct ShellVerification {
    pub(super) outcome: ShellOutcome,
    pub(super) trace: ShellTrace,
}

pub(super) enum ShellOutcome {
    Resolved(ResolvedTarget),
    Unsupported,
    Ambiguous,
    Unavailable,
}

#[allow(dead_code)] // Commit 6 serializes these reasons into the labeled resolver corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ShellTrace {
    Resolved { shell_windows: i32, view_items: u32 },
    Rejected(ShellRejection),
}

#[allow(dead_code)] // HRESULTs and counts are retained as bounded diagnostic evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ShellRejection {
    UnsupportedCandidatePath,
    ShellWindowsUnavailable(i32),
    InvalidShellWindowCount(i32),
    ShellWindowLimitExceeded(i32),
    PointerWindowUnavailable,
    PointerLeftForegroundExplorer,
    ShellWindowItemFailed { index: i32, code: i32 },
    RelevantViewFailed { index: i32, code: i32 },
    NoActiveViewAtPoint { inspected: i32 },
    MultipleActiveViews,
    NativeWindowOutsideView,
    ViewItemsFailed(i32),
    ViewItemLimitExceeded(u32),
    ViewItemFailed { index: u32, code: i32 },
    ViewItemPathFailed { index: u32, code: i32 },
    ViewItemPathMalformed { index: u32 },
    NoMatchingFilesystemItem { inspected: u32 },
    MultipleMatchingFilesystemItems,
    MatchingItemAttributesFailed(i32),
    MatchingItemIsNotAFile,
}

pub(super) fn verify(
    point: PhysicalScreenPoint,
    evidence: CandidateEvidence<'_>,
) -> ShellVerification {
    if !is_supported_local_path(evidence.path_units) {
        return rejected(
            ShellOutcome::Unsupported,
            ShellRejection::UnsupportedCandidatePath,
        );
    }

    // SAFETY: the caller owns a live MTA for the duration of this function. ShellWindows is a
    // system COM class, no aggregation is requested, and every returned interface remains local
    // to this call/apartment.
    let shell_windows: IShellWindows =
        match unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) } {
            Ok(shell_windows) => shell_windows,
            Err(error) => {
                return rejected(
                    ShellOutcome::Unavailable,
                    ShellRejection::ShellWindowsUnavailable(error.code().0),
                );
            }
        };

    let (folder_view, shell_window_count) =
        match active_folder_view(&shell_windows, point, evidence.item_native_window) {
            Ok(view) => view,
            Err(reason) => {
                let outcome = match reason {
                    ShellRejection::MultipleActiveViews => ShellOutcome::Ambiguous,
                    _ => ShellOutcome::Unavailable,
                };
                return rejected(outcome, reason);
            }
        };

    match matching_item(&folder_view, evidence.path_units) {
        Ok((target, view_items)) => ShellVerification {
            outcome: ShellOutcome::Resolved(target),
            trace: ShellTrace::Resolved {
                shell_windows: shell_window_count,
                view_items,
            },
        },
        Err(reason) => {
            let outcome = match reason {
                ShellRejection::MultipleMatchingFilesystemItems => ShellOutcome::Ambiguous,
                ShellRejection::MatchingItemIsNotAFile => ShellOutcome::Unsupported,
                _ => ShellOutcome::Unavailable,
            };
            rejected(outcome, reason)
        }
    }
}

fn rejected(outcome: ShellOutcome, reason: ShellRejection) -> ShellVerification {
    ShellVerification {
        outcome,
        trace: ShellTrace::Rejected(reason),
    }
}

fn active_folder_view(
    shell_windows: &IShellWindows,
    point: PhysicalScreenPoint,
    item_native_window: usize,
) -> Result<(IFolderView2, i32), ShellRejection> {
    // SAFETY: every method is called on an apartment-local interface. Window handles are copied
    // values and are revalidated before use. No borrowed pointer outlives this function.
    unsafe {
        let count = shell_windows
            .Count()
            .map_err(|error| ShellRejection::ShellWindowsUnavailable(error.code().0))?;
        if count < 0 {
            return Err(ShellRejection::InvalidShellWindowCount(count));
        }
        if count > MAX_SHELL_WINDOWS {
            return Err(ShellRejection::ShellWindowLimitExceeded(count));
        }

        let screen_point = POINT {
            x: point.x,
            y: point.y,
        };
        let pointer_window = WindowFromPoint(screen_point);
        if pointer_window.0.is_null() {
            return Err(ShellRejection::PointerWindowUnavailable);
        }
        let pointer_root = GetAncestor(pointer_window, GA_ROOT);
        let foreground = GetForegroundWindow();
        if pointer_root.0.is_null() || foreground != pointer_root {
            return Err(ShellRejection::PointerLeftForegroundExplorer);
        }

        let mut selected: Option<(HWND, IFolderView2)> = None;
        for index in 0..count {
            let dispatch = shell_windows.Item(&VARIANT::from(index)).map_err(|error| {
                ShellRejection::ShellWindowItemFailed {
                    index,
                    code: error.code().0,
                }
            })?;
            let Ok(browser) = dispatch.cast::<IWebBrowser2>() else {
                continue;
            };
            let Ok(browser_handle) = browser.HWND() else {
                continue;
            };
            let browser_window = HWND(browser_handle.0 as *mut c_void);
            if browser_window != pointer_root {
                continue;
            }

            let (shell_view, folder_view) = relevant_folder_view(&browser, index)?;
            let view_window =
                shell_view
                    .GetWindow()
                    .map_err(|error| ShellRejection::RelevantViewFailed {
                        index,
                        code: error.code().0,
                    })?;
            if view_window.0.is_null()
                || !IsWindowVisible(view_window).as_bool()
                || IsIconic(view_window).as_bool()
            {
                continue;
            }

            let mut view_rect = RECT::default();
            GetWindowRect(view_window, &mut view_rect).map_err(|error| {
                ShellRejection::RelevantViewFailed {
                    index,
                    code: error.code().0,
                }
            })?;
            let point_inside = rect_contains(view_rect, point);
            let pointer_inside =
                pointer_window == view_window || IsChild(view_window, pointer_window).as_bool();
            if !point_inside || !pointer_inside {
                continue;
            }

            if item_native_window != 0 {
                let item_window = HWND(item_native_window as *mut c_void);
                if item_window != view_window && !IsChild(view_window, item_window).as_bool() {
                    return Err(ShellRejection::NativeWindowOutsideView);
                }
            }

            if let Some((selected_window, _)) = &selected {
                if *selected_window != view_window {
                    return Err(ShellRejection::MultipleActiveViews);
                }
            } else {
                selected = Some((view_window, folder_view));
            }
        }

        selected
            .map(|(_, view)| (view, count))
            .ok_or(ShellRejection::NoActiveViewAtPoint { inspected: count })
    }
}

unsafe fn relevant_folder_view(
    browser: &IWebBrowser2,
    index: i32,
) -> Result<(IShellView, IFolderView2), ShellRejection> {
    // SAFETY: browser is an apartment-local ShellWindows entry. QueryService and the active-view
    // query return owned COM interfaces; casting preserves COM reference ownership.
    unsafe {
        let service_provider = browser.cast::<IServiceProvider>().map_err(|error| {
            ShellRejection::RelevantViewFailed {
                index,
                code: error.code().0,
            }
        })?;
        let shell_browser: IShellBrowser = service_provider
            .QueryService(&SID_STopLevelBrowser)
            .map_err(|error| ShellRejection::RelevantViewFailed {
                index,
                code: error.code().0,
            })?;
        let shell_view = shell_browser.QueryActiveShellView().map_err(|error| {
            ShellRejection::RelevantViewFailed {
                index,
                code: error.code().0,
            }
        })?;
        let folder_view = shell_view.cast::<IFolderView2>().map_err(|error| {
            ShellRejection::RelevantViewFailed {
                index,
                code: error.code().0,
            }
        })?;
        Ok((shell_view, folder_view))
    }
}

fn matching_item(
    folder_view: &IFolderView2,
    candidate_path: &[u16],
) -> Result<(ResolvedTarget, u32), ShellRejection> {
    // SAFETY: the view is apartment-local. The returned array/items are binding-owned COM
    // interfaces. GetDisplayName allocates with the COM task allocator; OwnedShellPath frees every
    // successful result, including error/early-return paths below.
    unsafe {
        let items: IShellItemArray = folder_view
            .Items(SVGIO_ALLVIEW)
            .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
        let count = items
            .GetCount()
            .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
        if count > MAX_VIEW_ITEMS {
            return Err(ShellRejection::ViewItemLimitExceeded(count));
        }

        let mut matched: Option<IShellItem> = None;
        for index in 0..count {
            let item = items
                .GetItemAt(index)
                .map_err(|error| ShellRejection::ViewItemFailed {
                    index,
                    code: error.code().0,
                })?;
            let path =
                OwnedShellPath::new(item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|error| {
                    ShellRejection::ViewItemPathFailed {
                        index,
                        code: error.code().0,
                    }
                })?);
            let units = path
                .units()
                .ok_or(ShellRejection::ViewItemPathMalformed { index })?;
            if units != candidate_path {
                continue;
            }
            if matched.is_some() {
                return Err(ShellRejection::MultipleMatchingFilesystemItems);
            }
            matched = Some(item);
        }

        let item = matched.ok_or(ShellRejection::NoMatchingFilesystemItem { inspected: count })?;
        let attributes = item
            .GetAttributes(SFGAO_FILESYSTEM | SFGAO_FOLDER)
            .map_err(|error| ShellRejection::MatchingItemAttributesFailed(error.code().0))?;
        if !attributes.contains(SFGAO_FILESYSTEM) || attributes.contains(SFGAO_FOLDER) {
            return Err(ShellRejection::MatchingItemIsNotAFile);
        }

        let path = PathBuf::from(OsString::from_wide(candidate_path));
        Ok((ResolvedTarget::new(path), count))
    }
}

fn rect_contains(rect: RECT, point: PhysicalScreenPoint) -> bool {
    rect.left < rect.right
        && rect.top < rect.bottom
        && rect.left <= point.x
        && point.x < rect.right
        && rect.top <= point.y
        && point.y < rect.bottom
}

fn is_supported_local_path(path: &[u16]) -> bool {
    !path.contains(&0)
        && path.len() >= 3
        && is_ascii_letter(path[0])
        && path[1] == u16::from(b':')
        && matches!(path[2], 0x2f | 0x5c)
}

fn is_ascii_letter(value: u16) -> bool {
    matches!(value, 0x41..=0x5a | 0x61..=0x7a)
}

struct OwnedShellPath(PWSTR);

impl OwnedShellPath {
    fn new(value: PWSTR) -> Self {
        Self(value)
    }

    unsafe fn units(&self) -> Option<&[u16]> {
        if self.0.0.is_null() {
            return None;
        }

        for length in 0..=MAX_FILESYSTEM_PATH_UNITS {
            // SAFETY: IShellItem::GetDisplayName promises a valid null-terminated task-allocated
            // string. The scan is additionally capped at the Windows extended-path ceiling.
            if unsafe { *self.0.0.add(length) } == 0 {
                // SAFETY: the preceding bounded scan proved that these initialized units precede
                // the terminator in the live task allocation.
                return Some(unsafe { std::slice::from_raw_parts(self.0.0, length) });
            }
        }
        None
    }
}

impl Drop for OwnedShellPath {
    fn drop(&mut self) {
        // SAFETY: GetDisplayName transfers one CoTaskMemAlloc-compatible pointer to the caller.
        // This owner frees it exactly once; CoTaskMemFree accepts null as well.
        unsafe {
            CoTaskMemFree(Some(self.0.0.cast()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_supported_local_path, rect_contains};
    use crate::hover::PhysicalScreenPoint;
    use windows::Win32::Foundation::RECT;

    #[test]
    fn local_path_policy_accepts_only_drive_absolute_values() {
        let wide = |value: &str| value.encode_utf16().collect::<Vec<_>>();

        assert!(is_supported_local_path(&wide(r"C:\Users\file.txt")));
        assert!(is_supported_local_path(&wide("d:/file.txt")));
        assert!(!is_supported_local_path(&wide(r"relative\file.txt")));
        assert!(!is_supported_local_path(&wide(r"\\server\share\file.txt")));
        assert!(!is_supported_local_path(&wide(r"\\?\C:\file.txt")));
        assert!(!is_supported_local_path(&[
            u16::from(b'C'),
            u16::from(b':'),
            0x5c,
            0
        ]));
    }

    #[test]
    fn shell_view_rectangles_use_ordered_half_open_bounds() {
        let rect = RECT {
            left: -100,
            top: -50,
            right: 100,
            bottom: 50,
        };
        assert!(rect_contains(rect, PhysicalScreenPoint::new(-100, -50)));
        assert!(rect_contains(rect, PhysicalScreenPoint::new(99, 49)));
        assert!(!rect_contains(rect, PhysicalScreenPoint::new(100, 0)));
        assert!(!rect_contains(rect, PhysicalScreenPoint::new(0, 50)));
        assert!(!rect_contains(
            RECT {
                left: 1,
                top: 0,
                right: 1,
                bottom: 2,
            },
            PhysicalScreenPoint::new(1, 1)
        ));
    }
}
