use windows::{
    Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MessageBoxW,
    },
    core::PCWSTR,
};

pub(crate) fn show_information(title: &str, message: &str) {
    show(title, message, MB_OK | MB_ICONINFORMATION);
}

pub(crate) fn show_error(title: &str, message: &str) {
    show(title, message, MB_OK | MB_ICONERROR);
}

fn show(title: &str, message: &str, style: MESSAGEBOX_STYLE) {
    let title = wide_z(title);
    let message = wide_z(message);
    // SAFETY: Both strings are terminated and remain alive for this synchronous call. A null owner
    // is appropriate before the application's hidden coordinator window exists.
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
        );
    }
}

fn wide_z(value: &str) -> Vec<u16> {
    value
        .chars()
        .map(|scalar| if scalar == '\0' { '\u{fffd}' } else { scalar })
        .collect::<String>()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::wide_z;

    #[test]
    fn dialog_text_is_terminated_and_cannot_be_truncated_by_an_embedded_null() {
        assert_eq!(
            wide_z("Cursor\0Peek"),
            [
                'C' as u16, 'u' as u16, 'r' as u16, 's' as u16, 'o' as u16, 'r' as u16, 0xfffd,
                'P' as u16, 'e' as u16, 'e' as u16, 'k' as u16, 0,
            ]
        );
    }
}
