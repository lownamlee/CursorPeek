use std::{
    error::Error,
    fmt,
    mem::{size_of, size_of_val},
    os::windows::io::{AsRawHandle, OwnedHandle},
};

use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{
            SystemServices::{
                PROCESS_MITIGATION_ASLR_POLICY, PROCESS_MITIGATION_DEP_POLICY,
                PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY,
            },
            Threading::{
                GetCurrentProcess, GetProcessMitigationPolicy, ProcessASLRPolicy, ProcessDEPPolicy,
                ProcessExtensionPointDisablePolicy, ProcessMitigationOptionsMask,
            },
        },
    },
    core::Error as WindowsError,
};

#[cfg(not(target_arch = "x86_64"))]
compile_error!("CursorPeek's worker mitigation policy currently supports only x86_64 Windows");

// These u64 values are the creation-policy constants documented in WinBase.h but not projected
// by windows 0.62.2. DEP and SEHOP are documented defaults for native x64 applications and their
// creation flags are intentionally omitted; in particular, x64 Windows does not advertise the
// SEHOP creation bit through ProcessMitigationOptionsMask.
const CREATION_MITIGATION_HEAP_TERMINATE_ALWAYS_ON: u64 = 1 << 12;
const CREATION_MITIGATION_BOTTOM_UP_ASLR_ALWAYS_ON: u64 = 1 << 16;
const CREATION_MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON: u64 = 1 << 20;
const CREATION_MITIGATION_EXTENSION_POINT_DISABLE_ALWAYS_ON: u64 = 1 << 32;
const REQUIRED_CREATION_MITIGATION_POLICY: u64 = CREATION_MITIGATION_HEAP_TERMINATE_ALWAYS_ON
    | CREATION_MITIGATION_BOTTOM_UP_ASLR_ALWAYS_ON
    | CREATION_MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON
    | CREATION_MITIGATION_EXTENSION_POINT_DISABLE_ALWAYS_ON;

const DEP_ENABLE_FLAG: u32 = 1 << 0;
const ASLR_BOTTOM_UP_FLAG: u32 = 1 << 0;
const ASLR_HIGH_ENTROPY_FLAG: u32 = 1 << 2;
const EXTENSION_POINT_DISABLE_FLAG: u32 = 1 << 0;

pub(super) struct CreationMitigationPolicy(u64);

impl CreationMitigationPolicy {
    pub(super) fn required() -> Result<Self, MitigationError> {
        let mut supported = [0_u64; 2];
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle for the caller. supported is the
        // documented two-word writable buffer for ProcessMitigationOptionsMask on the Windows 10
        // 22H2-or-newer project floor.
        unsafe {
            GetProcessMitigationPolicy(
                GetCurrentProcess(),
                ProcessMitigationOptionsMask,
                supported.as_mut_ptr().cast(),
                size_of_val(&supported),
            )?;
        }

        let missing = missing_required_mitigations(supported[0]);
        if missing == 0 {
            Ok(Self(REQUIRED_CREATION_MITIGATION_POLICY))
        } else {
            Err(MitigationError::UnsupportedPolicy(missing))
        }
    }

    pub(super) fn as_raw(&self) -> &u64 {
        &self.0
    }

    pub(super) fn verify_process(&self, process: &OwnedHandle) -> Result<(), MitigationError> {
        let process = HANDLE(process.as_raw_handle());
        let mut dep = PROCESS_MITIGATION_DEP_POLICY::default();
        let mut aslr = PROCESS_MITIGATION_ASLR_POLICY::default();
        let mut extension_points = PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY::default();

        // SAFETY: process is the live process handle returned by CreateProcessW and has query
        // access. Each buffer is the exact writable structure and byte count for its policy.
        unsafe {
            GetProcessMitigationPolicy(
                process,
                ProcessDEPPolicy,
                (&raw mut dep).cast(),
                size_of::<PROCESS_MITIGATION_DEP_POLICY>(),
            )?;
            GetProcessMitigationPolicy(
                process,
                ProcessASLRPolicy,
                (&raw mut aslr).cast(),
                size_of::<PROCESS_MITIGATION_ASLR_POLICY>(),
            )?;
            GetProcessMitigationPolicy(
                process,
                ProcessExtensionPointDisablePolicy,
                (&raw mut extension_points).cast(),
                size_of::<PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY>(),
            )?;
        }

        // SAFETY: each union was initialized by GetProcessMitigationPolicy for its matching
        // policy, so reading the documented Flags view is valid.
        let snapshot = unsafe {
            MitigationSnapshot {
                dep_flags: dep.Anonymous.Flags,
                aslr_flags: aslr.Anonymous.Flags,
                extension_point_flags: extension_points.Anonymous.Flags,
            }
        };
        snapshot.validate()
    }
}

fn missing_required_mitigations(supported: u64) -> u64 {
    REQUIRED_CREATION_MITIGATION_POLICY & !supported
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MitigationSnapshot {
    dep_flags: u32,
    aslr_flags: u32,
    extension_point_flags: u32,
}

impl MitigationSnapshot {
    fn validate(self) -> Result<(), MitigationError> {
        require_mitigation_flags("DEP", self.dep_flags, DEP_ENABLE_FLAG)?;
        require_mitigation_flags(
            "ASLR",
            self.aslr_flags,
            ASLR_BOTTOM_UP_FLAG | ASLR_HIGH_ENTROPY_FLAG,
        )?;
        require_mitigation_flags(
            "extension-point disable",
            self.extension_point_flags,
            EXTENSION_POINT_DISABLE_FLAG,
        )
    }
}

fn require_mitigation_flags(
    policy: &'static str,
    actual: u32,
    required: u32,
) -> Result<(), MitigationError> {
    if actual & required == required {
        Ok(())
    } else {
        Err(MitigationError::NotApplied {
            policy,
            required,
            actual,
        })
    }
}

#[derive(Debug)]
pub(crate) enum MitigationError {
    Native(WindowsError),
    UnsupportedPolicy(u64),
    NotApplied {
        policy: &'static str,
        required: u32,
        actual: u32,
    },
}

impl fmt::Display for MitigationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(error) => write!(formatter, "{error}"),
            Self::UnsupportedPolicy(missing) => write!(
                formatter,
                "Windows does not support required worker mitigation bits {missing:#018x}"
            ),
            Self::NotApplied {
                policy,
                required,
                actual,
            } => write!(
                formatter,
                "worker {policy} mitigation flags are {actual:#010x}; required {required:#010x}"
            ),
        }
    }
}

impl Error for MitigationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            Self::UnsupportedPolicy(_) | Self::NotApplied { .. } => None,
        }
    }
}

impl From<WindowsError> for MitigationError {
    fn from(error: WindowsError) -> Self {
        Self::Native(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ASLR_BOTTOM_UP_FLAG, ASLR_HIGH_ENTROPY_FLAG, DEP_ENABLE_FLAG, EXTENSION_POINT_DISABLE_FLAG,
        MitigationError, MitigationSnapshot, REQUIRED_CREATION_MITIGATION_POLICY,
        missing_required_mitigations,
    };

    const APPLIED: MitigationSnapshot = MitigationSnapshot {
        dep_flags: DEP_ENABLE_FLAG,
        aslr_flags: ASLR_BOTTOM_UP_FLAG | ASLR_HIGH_ENTROPY_FLAG,
        extension_point_flags: EXTENSION_POINT_DISABLE_FLAG,
    };

    #[test]
    fn creation_policy_contains_the_frozen_four_protections() {
        assert_eq!(REQUIRED_CREATION_MITIGATION_POLICY, 0x0000_0001_0011_1000);
    }

    #[test]
    fn supported_policy_accepts_extra_host_bits() {
        assert_eq!(
            missing_required_mitigations(REQUIRED_CREATION_MITIGATION_POLICY | (1 << 63)),
            0
        );
    }

    #[test]
    fn unsupported_policy_reports_only_required_missing_bits() {
        let missing = (1 << 12) | (1 << 32);
        let supported = REQUIRED_CREATION_MITIGATION_POLICY & !missing;

        assert_eq!(missing_required_mitigations(supported), missing);
    }

    #[test]
    fn queryable_mitigation_snapshot_accepts_every_required_flag() {
        assert!(APPLIED.validate().is_ok());
    }

    #[test]
    fn queryable_mitigation_snapshot_rejects_each_missing_group() {
        for snapshot in [
            MitigationSnapshot {
                dep_flags: 0,
                ..APPLIED
            },
            MitigationSnapshot {
                aslr_flags: ASLR_HIGH_ENTROPY_FLAG,
                ..APPLIED
            },
            MitigationSnapshot {
                aslr_flags: ASLR_BOTTOM_UP_FLAG,
                ..APPLIED
            },
            MitigationSnapshot {
                extension_point_flags: 0,
                ..APPLIED
            },
        ] {
            assert!(matches!(
                snapshot.validate(),
                Err(MitigationError::NotApplied { .. })
            ));
        }
    }
}
