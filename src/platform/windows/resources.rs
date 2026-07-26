use windows::{
    Win32::{
        Foundation::HINSTANCE,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            GetSystemMetrics, HICON, IMAGE_ICON, LR_SHARED, LoadImageW, SM_CXSMICON, SM_CYSMICON,
        },
    },
    core::{PCWSTR, Result},
};

pub(crate) const APPLICATION_ICON_RESOURCE_ID: u16 = 101;

pub(crate) fn load_small_application_icon() -> Result<HICON> {
    // SAFETY: A null module name returns the current executable's borrowed module handle. Resource
    // 101 is a checked-in ICON group, and the requested dimensions are standard small-icon system
    // metrics. LR_SHARED makes the returned HICON system-owned for the process lifetime.
    unsafe {
        let instance = HINSTANCE::from(GetModuleHandleW(None)?);
        let width = GetSystemMetrics(SM_CXSMICON);
        let height = GetSystemMetrics(SM_CYSMICON);
        let resource = PCWSTR(APPLICATION_ICON_RESOURCE_ID as usize as *const u16);
        let handle = LoadImageW(
            Some(instance),
            resource,
            IMAGE_ICON,
            width,
            height,
            LR_SHARED,
        )?;
        Ok(HICON(handle.0))
    }
}

#[cfg(test)]
mod tests {
    use super::load_small_application_icon;

    #[test]
    fn embedded_application_icon_loads_at_the_system_small_icon_size() {
        load_small_application_icon().expect("the embedded application icon should load");
    }
}
