use std::{env, os::windows::ffi::OsStrExt, path::Path};

use windows::{
    Win32::{
        Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_UNSUPPORTED_TYPE, WIN32_ERROR,
        },
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
            RRF_ZEROONFAILURE, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
        },
    },
    core::{Error, Owned, PCWSTR, Result, w},
};

const MAX_REGISTRY_COMMAND_BYTES: u32 = 64 * 1024;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("CursorPeek");

pub(crate) struct StartupRegistration {
    command: Vec<u16>,
}

impl StartupRegistration {
    pub(crate) fn for_current_executable() -> Result<Self> {
        let executable = env::current_exe().map_err(|_| Error::from_thread())?;
        let command = quoted_executable_command(&executable)?;
        Ok(Self { command })
    }

    pub(crate) fn reconcile(&self, enabled: bool) -> Result<()> {
        if enabled {
            self.write()
        } else {
            self.remove_if_owned()
        }
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.reconcile(enabled)
    }

    fn write(&self) -> Result<()> {
        let mut raw_key = HKEY::default();
        // SAFETY: HKEY_CURRENT_USER is predefined, both names are terminated static UTF-16, and
        // raw_key is writable storage that receives one owned handle on success.
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut raw_key,
                None,
            )
            .ok()?;
        }
        // SAFETY: RegCreateKeyExW returned a new key handle and this guard is its only owner.
        let key = unsafe { Owned::<HKEY>::new(raw_key) };
        let bytes = utf16_as_bytes(&self.command);

        // SAFETY: The owned key has KEY_SET_VALUE access. VALUE_NAME and the command are
        // terminated; REG_SZ receives the complete UTF-16 byte slice including that terminator.
        unsafe { RegSetValueExW(*key, VALUE_NAME, None, REG_SZ, Some(bytes)).ok() }
    }

    fn remove_if_owned(&self) -> Result<()> {
        if self.read()?.as_deref() != Some(self.command.as_slice()) {
            return Ok(());
        }

        let mut raw_key = HKEY::default();
        // SAFETY: The predefined root and terminated subkey are valid. Missing keys are already
        // handled by read(); the returned handle is owned on success.
        unsafe {
            windows::Win32::System::Registry::RegOpenKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                KEY_SET_VALUE,
                &mut raw_key,
            )
            .ok()?;
        }
        // SAFETY: RegOpenKeyExW returned a new key handle and this guard is its only owner.
        let key = unsafe { Owned::<HKEY>::new(raw_key) };
        // SAFETY: The owned key permits value deletion and VALUE_NAME is terminated static UTF-16.
        let status = unsafe { RegDeleteValueW(*key, VALUE_NAME) };
        if is_missing(status) {
            Ok(())
        } else {
            status.ok()
        }
    }

    fn read(&self) -> Result<Option<Vec<u16>>> {
        let flags = RRF_RT_REG_SZ | RRF_ZEROONFAILURE;
        let mut bytes = 0_u32;
        // SAFETY: This size query supplies no data pointer and a live byte-count pointer. The root,
        // subkey, and value names are predefined or terminated static UTF-16.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                flags,
                None,
                None,
                Some(&mut bytes),
            )
        };
        if is_not_owned(status) {
            return Ok(None);
        }
        status.ok()?;
        if bytes == 0 || bytes > MAX_REGISTRY_COMMAND_BYTES || !bytes.is_multiple_of(2) {
            return Ok(None);
        }

        let mut units = vec![0_u16; usize::try_from(bytes / 2).map_err(|_| Error::from_thread())?];
        let mut copied = bytes;
        // SAFETY: units provides exactly `bytes` writable aligned bytes and copied starts at that
        // capacity. The same bounded REG_SZ-only query is repeated into the live allocation.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                flags,
                None,
                Some(units.as_mut_ptr().cast()),
                Some(&mut copied),
            )
        };
        if is_not_owned(status) {
            return Ok(None);
        }
        status.ok()?;
        if copied == 0 || copied > bytes || !copied.is_multiple_of(2) {
            return Ok(None);
        }
        units.truncate(usize::try_from(copied / 2).map_err(|_| Error::from_thread())?);
        if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
            return Ok(None);
        }
        Ok(Some(units))
    }
}

fn quoted_executable_command(path: &Path) -> Result<Vec<u16>> {
    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if units.is_empty() || units.contains(&0) || units.len() > 32_767 - 3 {
        return Err(Error::from_thread());
    }

    let mut command = Vec::with_capacity(units.len() + 3);
    command.push(u16::from(b'"'));
    command.extend_from_slice(&units);
    command.extend_from_slice(&[u16::from(b'"'), 0]);
    Ok(command)
}

fn utf16_as_bytes(units: &[u16]) -> &[u8] {
    // SAFETY: u16 has no invalid bit patterns. The returned slice borrows the same allocation,
    // uses its full checked byte length, and is consumed synchronously by RegSetValueExW.
    unsafe { std::slice::from_raw_parts(units.as_ptr().cast(), std::mem::size_of_val(units)) }
}

fn is_missing(status: WIN32_ERROR) -> bool {
    status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND
}

fn is_not_owned(status: WIN32_ERROR) -> bool {
    is_missing(status) || status == ERROR_UNSUPPORTED_TYPE
}

#[cfg(test)]
mod tests {
    use super::{quoted_executable_command, utf16_as_bytes};
    use std::path::Path;

    #[test]
    fn startup_command_always_quotes_the_exact_executable() {
        let command = quoted_executable_command(Path::new(r"C:\Apps With Spaces\CursorPeek.exe"))
            .expect("the path should produce a startup command");
        assert_eq!(
            String::from_utf16(&command[..command.len() - 1]).unwrap(),
            r#""C:\Apps With Spaces\CursorPeek.exe""#
        );
        assert_eq!(command.last(), Some(&0));
        assert_eq!(utf16_as_bytes(&command).len(), command.len() * 2);
    }

    #[test]
    fn startup_command_rejects_empty_and_embedded_null_paths() {
        assert!(quoted_executable_command(Path::new("")).is_err());
        assert!(quoted_executable_command(Path::new("bad\0path.exe")).is_err());
    }
}
