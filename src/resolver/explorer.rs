use std::{error::Error, fmt, time::Duration};

mod candidate;
mod shell;

use candidate::{
    BoundedLegacyValue, BoundedText, CachedElementMetadata, CachedMetadataError, CachedProperty,
    CachedRect, CandidateEvidenceError, ControlKind, MAX_ANCESTORS, RejectedTrace, RejectionReason,
    ResolutionTrace, WalkTermination, finish_trace,
};
use shell::{ShellOutcome, ShellTrace};
use windows::{
    Win32::{
        Foundation::POINT,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Accessibility::{
            CUIAutomation8, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
            IUIAutomationLegacyIAccessiblePattern, IUIAutomationTreeWalker, TreeScope_Element,
            UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId,
            UIA_LegacyIAccessiblePatternId, UIA_NamePropertyId, UIA_NativeWindowHandlePropertyId,
        },
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
    // Fields drop in declaration order: release every apartment-owned interface first.
    automation: IUIAutomation2,
    cache_request: IUIAutomationCacheRequest,
    control_walker: IUIAutomationTreeWalker,
    last_trace: Option<ExplorerTrace>,
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

        // SAFETY: all three interfaces are created by the live apartment-local automation client.
        // The cache request retains copied property/pattern identifiers only. TreeScope_Element
        // limits each lookup to the returned element; parents are fetched individually below.
        let (cache_request, control_walker) = unsafe {
            let cache_request = automation.CreateCacheRequest()?;
            cache_request.SetTreeScope(TreeScope_Element)?;
            for property in [
                UIA_ControlTypePropertyId,
                UIA_NamePropertyId,
                UIA_BoundingRectanglePropertyId,
                UIA_NativeWindowHandlePropertyId,
                UIA_AutomationIdPropertyId,
            ] {
                cache_request.AddProperty(property)?;
            }
            cache_request.AddPattern(UIA_LegacyIAccessiblePatternId)?;

            (cache_request, automation.ControlViewWalker()?)
        };

        Ok(Self {
            automation,
            cache_request,
            control_walker,
            last_trace: None,
            _apartment: apartment,
        })
    }

    fn inspect_point(&self, point: PhysicalScreenPoint) -> ResolutionTrace {
        // SAFETY: the resolver and all supplied UIA interfaces remain on their owning MTA. POINT
        // contains copied physical desktop coordinates, and the cache request is immutable here.
        let mut element = match unsafe {
            self.automation.ElementFromPointBuildCache(
                POINT {
                    x: point.x,
                    y: point.y,
                },
                &self.cache_request,
            )
        } {
            Ok(element) => element,
            Err(error) => {
                return ResolutionTrace::Rejected(RejectedTrace {
                    inspected: Vec::new(),
                    reason: RejectionReason::ElementLookupFailed(error.code().0),
                });
            }
        };

        let mut inspected = Vec::with_capacity(MAX_ANCESTORS + 1);
        let mut item_index = None;

        for depth in 0..=MAX_ANCESTORS {
            let metadata = match self.read_cached_metadata(&element, depth) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return finish_trace(
                        inspected,
                        item_index,
                        WalkTermination::CachedMetadataFailed(error),
                    );
                }
            };

            if item_index.is_none() && metadata.control_kind.is_item() {
                if !metadata.bounds.is_ordered() {
                    let bounds = metadata.bounds;
                    inspected.push(metadata);
                    return ResolutionTrace::Rejected(RejectedTrace {
                        inspected,
                        reason: RejectionReason::InvalidItemBounds { depth, bounds },
                    });
                }
                if !metadata.bounds.contains(point) {
                    let bounds = metadata.bounds;
                    inspected.push(metadata);
                    return ResolutionTrace::Rejected(RejectedTrace {
                        inspected,
                        reason: RejectionReason::PointOutsideItemBounds {
                            depth,
                            bounds,
                            point,
                        },
                    });
                }
                item_index = Some(inspected.len());
            }
            inspected.push(metadata);

            if depth == MAX_ANCESTORS {
                return finish_trace(inspected, item_index, WalkTermination::AncestorLimitReached);
            }

            // SAFETY: element, the walker, and the cache request belong to this MTA. The generated
            // binding owns the returned interface, which replaces the previous local only after the
            // call completes. At most MAX_ANCESTORS parent calls are issued.
            element = match unsafe {
                self.control_walker
                    .GetParentElementBuildCache(&element, &self.cache_request)
            } {
                Ok(parent) => parent,
                Err(error) => {
                    return finish_trace(
                        inspected,
                        item_index,
                        WalkTermination::ParentLookupFailed {
                            after_depth: depth,
                            code: error.code().0,
                        },
                    );
                }
            };
        }

        unreachable!("the bounded UI Automation walk always returns from the loop")
    }

    fn read_cached_metadata(
        &self,
        element: &IUIAutomationElement,
        depth: usize,
    ) -> Result<CachedElementMetadata, CachedMetadataError> {
        // SAFETY: ElementFromPointBuildCache or GetParentElementBuildCache populated each requested
        // property on this apartment-local element. Returned BSTR/interface values are binding-owned;
        // only bounded copies or scalar values escape this function.
        unsafe {
            let control_type = element
                .CachedControlType()
                .map_err(|error| cached_error(depth, CachedProperty::ControlType, error))?;
            let control_kind = ControlKind::from_raw(control_type.0);
            let name = element
                .CachedName()
                .map(|value| BoundedText::from_bstr(&value))
                .map_err(|error| cached_error(depth, CachedProperty::Name, error))?;
            let bounds = element
                .CachedBoundingRectangle()
                .map(CachedRect::from)
                .map_err(|error| cached_error(depth, CachedProperty::BoundingRectangle, error))?;
            let native_window = element
                .CachedNativeWindowHandle()
                .map(|window| window.0 as usize)
                .map_err(|error| cached_error(depth, CachedProperty::NativeWindowHandle, error))?;
            let automation_id = element
                .CachedAutomationId()
                .map(|value| BoundedText::from_bstr(&value))
                .map_err(|error| cached_error(depth, CachedProperty::AutomationId, error))?;
            let legacy_pattern = element
                .GetCachedPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )
                .ok();
            let legacy_value = if control_kind.is_item() {
                legacy_pattern
                    .as_ref()
                    .and_then(|pattern| pattern.CachedValue().ok())
                    .map(|value| BoundedLegacyValue::from_bstr(&value))
            } else {
                None
            };

            Ok(CachedElementMetadata {
                depth,
                control_kind,
                name,
                bounds,
                native_window,
                automation_id,
                has_legacy_pattern: legacy_pattern.is_some(),
                legacy_value,
            })
        }
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
    fn resolve(&mut self, point: PhysicalScreenPoint) -> ResolveOutcome {
        let uia = self.inspect_point(point);
        let (outcome, shell) = match &uia {
            ResolutionTrace::Rejected(_) => (
                ResolveOutcome::Unavailable,
                ShellStageTrace::NotAttemptedAfterUiaRejection,
            ),
            ResolutionTrace::Candidate(candidate) => match candidate.shell_evidence() {
                Ok(evidence) => {
                    let verification = shell::verify(point, evidence);
                    let outcome = match verification.outcome {
                        ShellOutcome::Resolved(target) => ResolveOutcome::Resolved(target),
                        ShellOutcome::Unsupported => ResolveOutcome::Unsupported,
                        ShellOutcome::Ambiguous => ResolveOutcome::Ambiguous,
                        ShellOutcome::Unavailable => ResolveOutcome::Unavailable,
                    };
                    (outcome, ShellStageTrace::Attempted(verification.trace))
                }
                Err(reason) => {
                    let outcome = match reason {
                        CandidateEvidenceError::MissingItemsViewAncestor => {
                            ResolveOutcome::Unsupported
                        }
                        CandidateEvidenceError::MissingLegacyValue
                        | CandidateEvidenceError::TruncatedLegacyValue => {
                            ResolveOutcome::Unavailable
                        }
                    };
                    (outcome, ShellStageTrace::NotAttempted(reason))
                }
            },
        };
        let trace = ExplorerTrace { uia, shell };
        debug_assert!(trace.invariant_holds(point));
        self.last_trace = Some(trace);
        outcome
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExplorerTrace {
    uia: ResolutionTrace,
    shell: ShellStageTrace,
}

impl ExplorerTrace {
    fn invariant_holds(&self, point: PhysicalScreenPoint) -> bool {
        self.uia.invariant_holds(point)
            && matches!(
                (&self.uia, self.shell),
                (
                    ResolutionTrace::Rejected(_),
                    ShellStageTrace::NotAttemptedAfterUiaRejection
                ) | (
                    ResolutionTrace::Candidate(_),
                    ShellStageTrace::NotAttempted(_)
                ) | (ResolutionTrace::Candidate(_), ShellStageTrace::Attempted(_))
            )
    }
}

#[allow(dead_code)] // Commit 6 emits the bounded stage trace through the corpus runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellStageTrace {
    NotAttemptedAfterUiaRejection,
    NotAttempted(CandidateEvidenceError),
    Attempted(ShellTrace),
}

fn cached_error(
    depth: usize,
    property: CachedProperty,
    error: WindowsError,
) -> CachedMetadataError {
    CachedMetadataError {
        depth,
        property,
        code: error.code().0,
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
    use super::{ExplorerResolver, ResolutionTrace, UI_AUTOMATION_TIMEOUT};
    use crate::platform::{ApartmentKind, ComApartment};
    use crate::{
        hover::PhysicalScreenPoint,
        resolver::{PointResolver, ResolveOutcome},
    };
    use std::thread;

    #[test]
    fn automation_is_configured_resolves_a_point_and_releases_inside_its_mta() {
        thread::spawn(|| {
            let mut resolver = ExplorerResolver::initialize()
                .expect("the dedicated MTA should create CUIAutomation8");
            let expected = u32::try_from(UI_AUTOMATION_TIMEOUT.as_millis()).unwrap();
            assert_eq!(
                resolver.configured_timeouts().unwrap(),
                (expected, expected)
            );

            let point = PhysicalScreenPoint::new(0, 0);
            assert!(matches!(
                resolver.resolve(point),
                ResolveOutcome::Unavailable
                    | ResolveOutcome::Unsupported
                    | ResolveOutcome::Ambiguous
            ));
            let trace = resolver
                .last_trace
                .as_ref()
                .expect("a point request should leave one structured trace");
            assert!(trace.invariant_holds(point));
            assert!(matches!(
                &trace.uia,
                ResolutionTrace::Candidate(_) | ResolutionTrace::Rejected(_)
            ));

            drop(resolver);

            let opposite = ComApartment::initialize(ApartmentKind::SingleThreaded)
                .expect("resolver teardown should fully release its owning MTA initialization");
            drop(opposite);
        })
        .join()
        .expect("the resolver apartment test thread should not panic");
    }
}
