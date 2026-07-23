use std::{marker::PhantomData, rc::Rc};

use windows::{
    Win32::System::WinRT::{
        RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED, RO_INIT_TYPE, RoInitialize, RoUninitialize,
    },
    core::Result,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApartmentKind {
    SingleThreaded,
    MultiThreaded,
}

impl ApartmentKind {
    fn runtime_type(self) -> RO_INIT_TYPE {
        match self {
            Self::SingleThreaded => RO_INIT_SINGLETHREADED,
            Self::MultiThreaded => RO_INIT_MULTITHREADED,
        }
    }
}

pub(crate) struct ComApartment {
    _thread_affinity: PhantomData<Rc<()>>,
}

impl ComApartment {
    pub(crate) fn initialize(kind: ApartmentKind) -> Result<Self> {
        // SAFETY: The selected documented apartment type initializes both Windows Runtime and
        // underlying COM use. A guard is constructed for every successful HRESULT, including
        // S_FALSE, so Drop balances this exact call on the current thread.
        unsafe {
            RoInitialize(kind.runtime_type())?;
        }

        Ok(Self {
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: `PhantomData<Rc<()>>` makes the guard neither Send nor Sync, so safe code cannot
        // move this destructor away from the thread whose successful RoInitialize call it
        // balances. This guard owns exactly one matching uninitialization.
        unsafe {
            RoUninitialize();
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
