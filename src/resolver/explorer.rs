use std::{error::Error, fmt, time::Duration};

use windows::{
    Win32::{
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Accessibility::{CUIAutomation8, IUIAutomation2},
    },
    core::Error as WindowsError,
};

use crate::{
    hover::PhysicalScreenPoint,
    platform::{ApartmentKind, ComApartment},
};

use super::{PointResolver, ResolveOutcome};

const UI_AUTOMATION_TIMEOUT: Duration = Duration::from_millis(500);

pub(crate) struct ExplorerResolver {
    // Fields drop in declaration order: release the apartment-owned interface first.
    automation: IUIAutomation2,
    _apartment: ComApartment,
}

impl ExplorerResolver {
    pub(crate) fn initialize() -> Result<Self, ResolverError> {
        let apartment = ComApartment::initialize(ApartmentKind::MultiThreaded)?;
        let timeout_ms = u32::try_from(UI_AUTOMATION_TIMEOUT.as_millis())
            .expect("the fixed UI Automation timeout fits DWORD milliseconds");

        // SAFETY: the current thread owns the live MTA guard above. CUIAutomation8 is an in-process
        // COM class whose default interface is IUIAutomation2; no aggregation is requested.
        let automation: IUIAutomation2 =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };

        // SAFETY: automation is a live interface created in this MTA and remains apartment-local.
        // Both setters accept one bounded DWORD millisecond value and retain no borrowed pointer.
        unsafe {
            automation.SetConnectionTimeout(timeout_ms)?;
            automation.SetTransactionTimeout(timeout_ms)?;
        }

        // Read the values back so a successful initialization proves the intended bounds are
        // active rather than merely proving that the setter calls returned success.
        // SAFETY: automation is still live and apartment-local; the getters write into storage
        // owned by the generated bindings and return copied DWORD values.
        let (connection_timeout_ms, transaction_timeout_ms) = unsafe {
            (
                automation.ConnectionTimeout()?,
                automation.TransactionTimeout()?,
            )
        };
        if connection_timeout_ms != timeout_ms || transaction_timeout_ms != timeout_ms {
            return Err(ResolverError::TimeoutsNotApplied {
                expected_ms: timeout_ms,
                connection_timeout_ms,
                transaction_timeout_ms,
            });
        }

        Ok(Self {
            automation,
            _apartment: apartment,
        })
    }

    #[cfg(test)]
    fn configured_timeouts(&self) -> Result<(u32, u32), ResolverError> {
        // SAFETY: the resolver and its interface never leave the test's owning MTA thread.
        unsafe {
            Ok((
                self.automation.ConnectionTimeout()?,
                self.automation.TransactionTimeout()?,
            ))
        }
    }
}

impl PointResolver for ExplorerResolver {
    fn resolve(&mut self, _point: PhysicalScreenPoint) -> ResolveOutcome {
        // Point lookup and candidate classification belong to the next checkpoint. Retaining a
        // reference here makes the initialized apartment-owned client part of the real worker
        // request path without claiming that any item has been resolved.
        let _ = &self.automation;
        ResolveOutcome::Unavailable
    }
}

#[derive(Debug)]
pub(crate) enum ResolverError {
    Windows(WindowsError),
    TimeoutsNotApplied {
        expected_ms: u32,
        connection_timeout_ms: u32,
        transaction_timeout_ms: u32,
    },
}

impl fmt::Display for ResolverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows(error) => write!(formatter, "{error}"),
            Self::TimeoutsNotApplied {
                expected_ms,
                connection_timeout_ms,
                transaction_timeout_ms,
            } => write!(
                formatter,
                "UI Automation timeout verification failed: expected {expected_ms} ms, \
                 connection={connection_timeout_ms} ms, transaction={transaction_timeout_ms} ms"
            ),
        }
    }
}

impl Error for ResolverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Windows(error) => Some(error),
            Self::TimeoutsNotApplied { .. } => None,
        }
    }
}

impl From<WindowsError> for ResolverError {
    fn from(error: WindowsError) -> Self {
        Self::Windows(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{ExplorerResolver, UI_AUTOMATION_TIMEOUT};
    use crate::platform::{ApartmentKind, ComApartment};
    use std::thread;

    #[test]
    fn automation_is_configured_and_released_inside_its_mta() {
        thread::spawn(|| {
            let resolver = ExplorerResolver::initialize()
                .expect("the dedicated MTA should create CUIAutomation8");
            let expected = u32::try_from(UI_AUTOMATION_TIMEOUT.as_millis()).unwrap();
            assert_eq!(
                resolver.configured_timeouts().unwrap(),
                (expected, expected)
            );

            drop(resolver);

            let opposite = ComApartment::initialize(ApartmentKind::SingleThreaded)
                .expect("resolver teardown should fully release its owning MTA initialization");
            drop(opposite);
        })
        .join()
        .expect("the resolver apartment test thread should not panic");
    }
}
