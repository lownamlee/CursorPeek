use std::{marker::PhantomData, rc::Rc};

use windows::{
    core::Result,
    Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
        COINIT_MULTITHREADED,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApartmentKind {
    SingleThreaded,
    MultiThreaded,
}

impl ApartmentKind {
    fn flags(self) -> COINIT {
        match self {
            Self::SingleThreaded => COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            Self::MultiThreaded => COINIT_MULTITHREADED | COINIT_DISABLE_OLE1DDE,
        }
    }
}

pub(crate) struct ComApartment {
    _thread_affinity: PhantomData<Rc<()>>,
}

impl ComApartment {
    pub(crate) fn initialize(kind: ApartmentKind) -> Result<Self> {
        // SAFETY: The reserved pointer is null as required, and `kind.flags()` contains only
        // documented COINIT flags. A guard is constructed for every successful HRESULT,
        // including S_FALSE, so Drop balances this exact call on the current thread.
        unsafe {
            CoInitializeEx(None, kind.flags()).ok()?;
        }

        Ok(Self {
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: `PhantomData<Rc<()>>` makes the guard neither Send nor Sync, so safe code cannot
        // move this destructor away from the thread whose successful CoInitializeEx call it
        // balances. This guard owns exactly one matching uninitialization.
        unsafe {
            CoUninitialize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ApartmentKind, ComApartment};
    use std::thread;

    #[test]
    fn repeated_successful_initialization_is_fully_balanced() {
        thread::spawn(|| {
            let first = ComApartment::initialize(ApartmentKind::SingleThreaded)
                .expect("the dedicated test thread should accept STA initialization");
            let second = ComApartment::initialize(ApartmentKind::SingleThreaded)
                .expect("the repeated compatible initialization should return S_FALSE success");

            drop(second);
            drop(first);

            let opposite = ComApartment::initialize(ApartmentKind::MultiThreaded)
                .expect("dropping both guards should allow a new apartment model");
            drop(opposite);
        })
        .join()
        .expect("the COM apartment test thread should not panic");
    }
}
