use std::{
    ffi::{OsString, c_void},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
};

use windows::{
    Win32::{
        Foundation::{HWND, POINT},
        System::{
            Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IServiceProvider},
            SystemServices::{SFGAO_FILESYSTEM, SFGAO_FOLDER},
            Variant::VARIANT,
        },
        UI::{
            Shell::{
                IFolderView2, IShellBrowser, IShellItem, IShellItemArray, IShellView,
                IShellWindows, IWebBrowser2, SID_STopLevelBrowser, SIGDN_FILESYSPATH,
                SIGDN_NORMALDISPLAY, SVGIO_ALLVIEW, ShellWindows,
            },
            WindowsAndMessaging::{
                GA_ROOT, GetAncestor, GetForegroundWindow, IsChild, IsIconic, IsWindowVisible,
                WindowFromPoint,
            },
        },
    },
    core::{IUnknown, Interface, PWSTR},
};

use crate::{hover::PhysicalScreenPoint, resolver::ResolvedTarget};
use cursorpeek_core::PhysicalScreenRect;

use super::candidate::CandidateEvidence;

const MAX_SHELL_WINDOWS: i32 = 64;
const MAX_VIEW_ITEMS: u32 = 10_000;
const MAX_FILESYSTEM_PATH_UNITS: usize = 32_767;

pub(super) struct ShellVerification {
    pub(super) outcome: ShellOutcome,
    pub(super) trace: ShellTrace,
}

#[derive(Clone)]
pub(super) struct ActiveFolderView {
    folder_view: IFolderView2,
    shell_browser: IShellBrowser,
    shell_view_identity: IUnknown,
    browser_window: HWND,
    pointer_window: HWND,
    shell_window_index: i32,
    shell_window_count: i32,
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
    UnsupportedResolvedPath,
    ShellWindowsUnavailable(i32),
    InvalidShellWindowCount(i32),
    ShellWindowLimitExceeded(i32),
    PointerWindowUnavailable,
    PointerLeftForegroundExplorer,
    ShellWindowItemFailed { index: i32, code: i32 },
    BrowserServiceProviderFailed { index: i32, code: i32 },
    TopLevelBrowserFailed { index: i32, code: i32 },
    ActiveShellViewFailed { index: i32, code: i32 },
    ActiveViewIdentityFailed { index: i32, code: i32 },
    ActiveViewChanged,
    FolderViewFailed { index: i32, code: i32 },
    NoActiveViewAtPoint { inspected: i32 },
    MultipleActiveViews,
    NativeWindowOutsideView,
    ViewItemsFailed(i32),
    InvalidViewItemCount(i32),
    ViewItemLimitExceeded(u32),
    CandidateItemIndexOutOfRange { index: u32, count: u32 },
    CandidateIdentityMismatch { index: u32 },
    NoCandidateViewAtPoint { inspected: i32 },
    CandidateRevalidationFailed(i32),
    CandidateChangedDuringVerification,
    InvalidTargetBounds,
    ViewItemFailed { index: u32, code: i32 },
    ViewItemPathFailed { index: u32, code: i32 },
    ViewItemPathMalformed { index: u32 },
    ViewItemDisplayNameFailed { index: u32, code: i32 },
    ViewItemDisplayNameMalformed { index: u32 },
    NoMatchingFilesystemItem { inspected: u32 },
    MultipleMatchingFilesystemItems,
    MatchingItemAttributesFailed(i32),
    MatchingItemIsNotAFile,
}

pub(super) fn create_collection() -> windows::core::Result<IShellWindows> {
    // SAFETY: the caller owns a live MTA for the duration of this function. ShellWindows is a
    // system COM class, no aggregation is requested, and the returned interface remains local to
    // the resolver apartment until it is released there.
    unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }
}

pub(super) fn select(
    shell_windows: &IShellWindows,
    cached: &mut Option<ActiveFolderView>,
    point: PhysicalScreenPoint,
    evidence: CandidateEvidence<'_>,
) -> Result<ActiveFolderView, ShellRejection> {
    if let Some(existing) = cached.as_ref() {
        match existing.try_reuse(shell_windows, point) {
            Ok(Some(active_view)) => {
                *cached = Some(active_view.clone());
                return Ok(active_view);
            }
            Ok(None) => {
                *cached = None;
            }
            Err(reason) => {
                *cached = None;
                return Err(reason);
            }
        }
    }

    let active_view = active_folder_view(shell_windows, point, evidence)?;
    *cached = Some(active_view.clone());
    Ok(active_view)
}

pub(super) fn selection_failure(reason: ShellRejection) -> ShellVerification {
    let outcome = match reason {
        ShellRejection::MultipleActiveViews => ShellOutcome::Ambiguous,
        _ => ShellOutcome::Unavailable,
    };
    rejected(outcome, reason)
}

pub(super) fn verify(
    active_view: &ActiveFolderView,
    point: PhysicalScreenPoint,
    evidence: CandidateEvidence<'_>,
) -> ShellVerification {
    if evidence
        .path_units
        .is_some_and(|path| !is_supported_local_path(path))
    {
        return rejected(
            ShellOutcome::Unsupported,
            ShellRejection::UnsupportedCandidatePath,
        );
    }

    if let Err(reason) = active_view.revalidate(point, evidence.item_native_window) {
        return rejected(ShellOutcome::Unavailable, reason);
    }

    let Some(target_bounds) = PhysicalScreenRect::try_new(
        evidence.item_bounds.left,
        evidence.item_bounds.top,
        evidence.item_bounds.right,
        evidence.item_bounds.bottom,
    ) else {
        return rejected(
            ShellOutcome::Unavailable,
            ShellRejection::InvalidTargetBounds,
        );
    };
    let resolution = matching_item(
        &active_view.folder_view,
        evidence.path_units,
        evidence.display_name_units,
        evidence.view_index,
        target_bounds,
    );
    if let Err(reason) = active_view.revalidate(point, evidence.item_native_window) {
        return rejected(ShellOutcome::Unavailable, reason);
    }

    match resolution {
        Ok((target, view_items)) => ShellVerification {
            outcome: ShellOutcome::Resolved(target),
            trace: ShellTrace::Resolved {
                shell_windows: active_view.shell_window_count,
                view_items,
            },
        },
        Err(reason) => {
            let outcome = match reason {
                ShellRejection::MultipleMatchingFilesystemItems
                | ShellRejection::CandidateIdentityMismatch { .. } => ShellOutcome::Ambiguous,
                ShellRejection::MatchingItemIsNotAFile
                | ShellRejection::UnsupportedResolvedPath => ShellOutcome::Unsupported,
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
    evidence: CandidateEvidence<'_>,
) -> Result<ActiveFolderView, ShellRejection> {
    // SAFETY: every method is called on an apartment-local interface. Window handles are copied
    // values and are revalidated before use. No borrowed pointer outlives this function.
    unsafe {
        let count = shell_window_count(shell_windows)?;

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

        let mut candidates = Vec::new();
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
            if !IsWindowVisible(browser_window).as_bool() || IsIconic(browser_window).as_bool() {
                continue;
            }

            let (shell_browser, shell_view, folder_view) = relevant_folder_view(&browser, index)?;
            let shell_view_identity = shell_view.cast::<IUnknown>().map_err(|error| {
                ShellRejection::ActiveViewIdentityFailed {
                    index,
                    code: error.code().0,
                }
            })?;

            if candidates.iter().any(|candidate: &ActiveFolderView| {
                Interface::as_raw(&candidate.shell_view_identity)
                    == Interface::as_raw(&shell_view_identity)
            }) {
                continue;
            }

            candidates.push(ActiveFolderView {
                folder_view,
                shell_browser,
                shell_view_identity,
                browser_window,
                pointer_window,
                shell_window_index: index,
                shell_window_count: count,
            });
        }

        if candidates.is_empty() {
            return Err(ShellRejection::NoActiveViewAtPoint { inspected: count });
        }
        if candidates.len() == 1 {
            return Ok(candidates
                .pop()
                .expect("the single-view branch retains one candidate"));
        }

        let inspected = i32::try_from(candidates.len())
            .expect("the Shell-window cap keeps the candidate count in i32");
        let mut matches = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            matches.push(view_matches_candidate(&candidate.folder_view, evidence)?);
        }

        match classify_view_matches(&matches) {
            ViewMatchSelection::None => Err(ShellRejection::NoCandidateViewAtPoint { inspected }),
            ViewMatchSelection::Unique(index) => Ok(candidates.swap_remove(index)),
            ViewMatchSelection::Multiple => Err(ShellRejection::MultipleActiveViews),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewMatchSelection {
    None,
    Unique(usize),
    Multiple,
}

fn classify_view_matches(matches: &[bool]) -> ViewMatchSelection {
    let mut selected = None;
    for (index, matched) in matches.iter().copied().enumerate() {
        if !matched {
            continue;
        }
        if selected.is_some() {
            return ViewMatchSelection::Multiple;
        }
        selected = Some(index);
    }
    selected.map_or(ViewMatchSelection::None, ViewMatchSelection::Unique)
}

unsafe fn relevant_folder_view(
    browser: &IWebBrowser2,
    index: i32,
) -> Result<(IShellBrowser, IShellView, IFolderView2), ShellRejection> {
    // SAFETY: browser is an apartment-local ShellWindows entry. QueryService and the active-view
    // query return owned COM interfaces; casting preserves COM reference ownership.
    unsafe {
        let service_provider = browser.cast::<IServiceProvider>().map_err(|error| {
            ShellRejection::BrowserServiceProviderFailed {
                index,
                code: error.code().0,
            }
        })?;
        let shell_browser: IShellBrowser = service_provider
            .QueryService(&SID_STopLevelBrowser)
            .map_err(|error| ShellRejection::TopLevelBrowserFailed {
                index,
                code: error.code().0,
            })?;
        let shell_view = shell_browser.QueryActiveShellView().map_err(|error| {
            ShellRejection::ActiveShellViewFailed {
                index,
                code: error.code().0,
            }
        })?;
        let folder_view = shell_view.cast::<IFolderView2>().map_err(|error| {
            ShellRejection::FolderViewFailed {
                index,
                code: error.code().0,
            }
        })?;
        Ok((shell_browser, shell_view, folder_view))
    }
}

fn shell_window_count(shell_windows: &IShellWindows) -> Result<i32, ShellRejection> {
    // SAFETY: the collection is live and apartment-local. Count returns one copied signed value.
    let count = unsafe { shell_windows.Count() }
        .map_err(|error| ShellRejection::ShellWindowsUnavailable(error.code().0))?;
    if count < 0 {
        return Err(ShellRejection::InvalidShellWindowCount(count));
    }
    if count > MAX_SHELL_WINDOWS {
        return Err(ShellRejection::ShellWindowLimitExceeded(count));
    }
    Ok(count)
}

impl ActiveFolderView {
    fn try_reuse(
        &self,
        shell_windows: &IShellWindows,
        point: PhysicalScreenPoint,
    ) -> Result<Option<Self>, ShellRejection> {
        // SAFETY: the cached interfaces remain on their owning MTA. A cache hit is allowed only
        // while the point still belongs to the same visible foreground browser, exactly one Shell
        // window is registered, and QueryActiveShellView returns the same controlling-IUnknown
        // identity. Multiple registrations may be same-frame tabs, so they always force full
        // evidence correlation instead of taking the cache.
        unsafe {
            let pointer_window = WindowFromPoint(POINT {
                x: point.x,
                y: point.y,
            });
            if pointer_window.0.is_null() {
                return Err(ShellRejection::PointerWindowUnavailable);
            }
            let pointer_root = GetAncestor(pointer_window, GA_ROOT);
            if pointer_root.0.is_null() || GetForegroundWindow() != pointer_root {
                return Err(ShellRejection::PointerLeftForegroundExplorer);
            }
            if pointer_root != self.browser_window
                || !IsWindowVisible(self.browser_window).as_bool()
                || IsIconic(self.browser_window).as_bool()
            {
                return Ok(None);
            }

            let count = shell_window_count(shell_windows)?;
            if count != self.shell_window_count || count != 1 {
                return Ok(None);
            }

            let Ok(current_view) = self.shell_browser.QueryActiveShellView() else {
                return Ok(None);
            };
            let Ok(current_identity) = current_view.cast::<IUnknown>() else {
                return Ok(None);
            };
            if Interface::as_raw(&current_identity) != Interface::as_raw(&self.shell_view_identity)
            {
                return Ok(None);
            }

            Ok(Some(Self {
                folder_view: self.folder_view.clone(),
                shell_browser: self.shell_browser.clone(),
                shell_view_identity: current_identity,
                browser_window: self.browser_window,
                pointer_window,
                shell_window_index: self.shell_window_index,
                shell_window_count: count,
            }))
        }
    }

    fn revalidate(
        &self,
        point: PhysicalScreenPoint,
        item_native_window: usize,
    ) -> Result<(), ShellRejection> {
        // SAFETY: all COM interfaces and HWND values remain on their owning resolver MTA. The
        // current view is compared through controlling-IUnknown identity; copied HWNDs are
        // revalidated before use.
        unsafe {
            let screen_point = POINT {
                x: point.x,
                y: point.y,
            };
            let pointer_window = WindowFromPoint(screen_point);
            if pointer_window.0.is_null() {
                return Err(ShellRejection::PointerWindowUnavailable);
            }
            let pointer_root = GetAncestor(pointer_window, GA_ROOT);
            if pointer_root.0.is_null()
                || pointer_root != self.browser_window
                || GetForegroundWindow() != pointer_root
            {
                return Err(ShellRejection::PointerLeftForegroundExplorer);
            }
            if pointer_window != self.pointer_window
                && !IsChild(self.pointer_window, pointer_window).as_bool()
                && !IsChild(pointer_window, self.pointer_window).as_bool()
            {
                return Err(ShellRejection::PointerLeftForegroundExplorer);
            }

            if item_native_window != 0 {
                let item_window = HWND(item_native_window as *mut c_void);
                if item_window != pointer_window
                    && !IsChild(item_window, pointer_window).as_bool()
                    && !IsChild(pointer_window, item_window).as_bool()
                {
                    return Err(ShellRejection::NativeWindowOutsideView);
                }
            }

            let current_view = self.shell_browser.QueryActiveShellView().map_err(|error| {
                ShellRejection::ActiveShellViewFailed {
                    index: self.shell_window_index,
                    code: error.code().0,
                }
            })?;
            let current_identity = current_view.cast::<IUnknown>().map_err(|error| {
                ShellRejection::ActiveViewIdentityFailed {
                    index: self.shell_window_index,
                    code: error.code().0,
                }
            })?;
            if Interface::as_raw(&current_identity) != Interface::as_raw(&self.shell_view_identity)
            {
                return Err(ShellRejection::ActiveViewChanged);
            }
            Ok(())
        }
    }
}

fn matching_item(
    folder_view: &IFolderView2,
    candidate_path: Option<&[u16]>,
    candidate_display_name: Option<&[u16]>,
    view_index: Option<u32>,
    target_bounds: PhysicalScreenRect,
) -> Result<(ResolvedTarget, u32), ShellRejection> {
    // SAFETY: the view is apartment-local. The returned array/items are binding-owned COM
    // interfaces. GetDisplayName allocates with the COM task allocator; OwnedShellPath frees every
    // successful result, including error/early-return paths below.
    unsafe {
        let count = view_item_count(folder_view)?;

        let (item, path_units) = if let Some(index) = view_index {
            if index >= count {
                return Err(ShellRejection::CandidateItemIndexOutOfRange { index, count });
            }
            let item = folder_view
                .GetItem::<IShellItem>(
                    i32::try_from(index).expect("the capped item index fits a signed view index"),
                )
                .map_err(|error| ShellRejection::ViewItemFailed {
                    index,
                    code: error.code().0,
                })?;
            if let Some(candidate) = candidate_display_name {
                let display_name = item_display_name(&item, index)?;
                if candidate != display_name {
                    return Err(ShellRejection::CandidateIdentityMismatch { index });
                }
            }
            let path_units = item_path(&item, index)?;
            if candidate_path.is_some_and(|candidate| candidate != path_units) {
                return Err(ShellRejection::CandidateIdentityMismatch { index });
            }
            (item, path_units)
        } else {
            let candidate_path =
                candidate_path.expect("candidate evidence always contains an index or a path");
            let items: IShellItemArray = folder_view
                .Items(SVGIO_ALLVIEW)
                .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
            let array_count = items
                .GetCount()
                .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
            if array_count > MAX_VIEW_ITEMS {
                return Err(ShellRejection::ViewItemLimitExceeded(array_count));
            }

            let mut matched: Option<(IShellItem, Vec<u16>)> = None;
            for index in 0..array_count {
                let item =
                    items
                        .GetItemAt(index)
                        .map_err(|error| ShellRejection::ViewItemFailed {
                            index,
                            code: error.code().0,
                        })?;
                let path_units = item_path(&item, index)?;
                if path_units != candidate_path {
                    continue;
                }
                if let Some(candidate_display_name) = candidate_display_name {
                    let display_name = item_display_name(&item, index)?;
                    if display_name != candidate_display_name {
                        return Err(ShellRejection::CandidateIdentityMismatch { index });
                    }
                }
                if matched.is_some() {
                    return Err(ShellRejection::MultipleMatchingFilesystemItems);
                }
                matched = Some((item, path_units));
            }

            matched.ok_or(ShellRejection::NoMatchingFilesystemItem {
                inspected: array_count,
            })?
        };

        if !is_supported_local_path(&path_units) {
            return Err(ShellRejection::UnsupportedResolvedPath);
        }
        let attributes = item
            .GetAttributes(SFGAO_FILESYSTEM | SFGAO_FOLDER)
            .map_err(|error| ShellRejection::MatchingItemAttributesFailed(error.code().0))?;
        if !attributes.contains(SFGAO_FILESYSTEM) || attributes.contains(SFGAO_FOLDER) {
            return Err(ShellRejection::MatchingItemIsNotAFile);
        }

        let path = PathBuf::from(OsString::from_wide(&path_units));
        Ok((ResolvedTarget::new(path, target_bounds), count))
    }
}

fn view_matches_candidate(
    folder_view: &IFolderView2,
    evidence: CandidateEvidence<'_>,
) -> Result<bool, ShellRejection> {
    // SAFETY: the folder view and returned Shell items stay on the resolver MTA. Every collection
    // traversal is bounded before an item is requested.
    unsafe {
        let count = view_item_count(folder_view)?;
        if let Some(index) = evidence.view_index {
            if index >= count {
                return Ok(false);
            }
            let item = folder_view
                .GetItem::<IShellItem>(
                    i32::try_from(index).expect("the capped item index fits a signed view index"),
                )
                .map_err(|error| ShellRejection::ViewItemFailed {
                    index,
                    code: error.code().0,
                })?;

            if let Some(candidate_display_name) = evidence.display_name_units {
                let display_name = item_display_name(&item, index)?;
                if !display_name_matches(Some(candidate_display_name), &display_name) {
                    return Ok(false);
                }
            }
            if let Some(candidate_path) = evidence.path_units {
                let path = item_path(&item, index)?;
                if path != candidate_path {
                    return Ok(false);
                }
            }
            return Ok(true);
        }

        let candidate_path = evidence
            .path_units
            .expect("candidate evidence without an index always retains a complete path");
        let items: IShellItemArray = folder_view
            .Items(SVGIO_ALLVIEW)
            .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
        let item_count = items
            .GetCount()
            .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
        if item_count > MAX_VIEW_ITEMS {
            return Err(ShellRejection::ViewItemLimitExceeded(item_count));
        }

        let mut matched = false;
        for index in 0..item_count {
            let item = items
                .GetItemAt(index)
                .map_err(|error| ShellRejection::ViewItemFailed {
                    index,
                    code: error.code().0,
                })?;
            if item_path(&item, index)? != candidate_path {
                continue;
            }
            if let Some(candidate_display_name) = evidence.display_name_units
                && !display_name_matches(
                    Some(candidate_display_name),
                    &item_display_name(&item, index)?,
                )
            {
                return Ok(false);
            }
            if matched {
                return Err(ShellRejection::MultipleMatchingFilesystemItems);
            }
            matched = true;
        }
        Ok(matched)
    }
}

fn display_name_matches(candidate: Option<&[u16]>, actual: &[u16]) -> bool {
    candidate.is_none_or(|candidate| candidate == actual)
}

fn view_item_count(folder_view: &IFolderView2) -> Result<u32, ShellRejection> {
    // SAFETY: the view is apartment-local and ItemCount returns one copied signed value.
    let raw_count = unsafe { folder_view.ItemCount(SVGIO_ALLVIEW) }
        .map_err(|error| ShellRejection::ViewItemsFailed(error.code().0))?;
    if raw_count < 0 {
        return Err(ShellRejection::InvalidViewItemCount(raw_count));
    }
    let count = u32::try_from(raw_count).expect("a nonnegative i32 fits u32");
    if count > MAX_VIEW_ITEMS {
        return Err(ShellRejection::ViewItemLimitExceeded(count));
    }
    Ok(count)
}

unsafe fn item_path(item: &IShellItem, index: u32) -> Result<Vec<u16>, ShellRejection> {
    // SAFETY: item is an apartment-local Shell interface. GetDisplayName transfers a
    // CoTaskMemAlloc-compatible string; OwnedShellPath frees it on every return path.
    unsafe {
        let path =
            OwnedShellPath::new(item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|error| {
                ShellRejection::ViewItemPathFailed {
                    index,
                    code: error.code().0,
                }
            })?);
        path.units()
            .map(<[u16]>::to_vec)
            .ok_or(ShellRejection::ViewItemPathMalformed { index })
    }
}

unsafe fn item_display_name(item: &IShellItem, index: u32) -> Result<Vec<u16>, ShellRejection> {
    // SAFETY: item is an apartment-local Shell interface. GetDisplayName transfers a
    // CoTaskMemAlloc-compatible string; OwnedShellPath frees it on every return path.
    unsafe {
        let display_name =
            OwnedShellPath::new(item.GetDisplayName(SIGDN_NORMALDISPLAY).map_err(|error| {
                ShellRejection::ViewItemDisplayNameFailed {
                    index,
                    code: error.code().0,
                }
            })?);
        display_name
            .units()
            .map(<[u16]>::to_vec)
            .ok_or(ShellRejection::ViewItemDisplayNameMalformed { index })
    }
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
    use super::{
        ViewMatchSelection, classify_view_matches, display_name_matches, is_supported_local_path,
    };

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
    fn same_frame_tabs_require_exactly_one_correlated_view() {
        assert_eq!(
            classify_view_matches(&[false, false]),
            ViewMatchSelection::None
        );
        assert_eq!(
            classify_view_matches(&[false, true, false]),
            ViewMatchSelection::Unique(1)
        );
        assert_eq!(
            classify_view_matches(&[true, false, true]),
            ViewMatchSelection::Multiple
        );
    }

    #[test]
    fn display_name_correlation_is_exact_and_optional() {
        let expected = "preview.txt".encode_utf16().collect::<Vec<_>>();
        let different = "Preview.txt".encode_utf16().collect::<Vec<_>>();

        assert!(display_name_matches(None, &different));
        assert!(display_name_matches(Some(&expected), &expected));
        assert!(!display_name_matches(Some(&expected), &different));
    }
}
