#[cfg(feature = "resolver-corpus")]
use std::path::PathBuf;
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
        System::Variant::{VARIANT, VT_I4, VariantClear},
        UI::Accessibility::{
            CUIAutomation8, CUIAutomationRegistrar, IUIAutomation2, IUIAutomationCacheRequest,
            IUIAutomationElement, IUIAutomationLegacyIAccessiblePattern, IUIAutomationRegistrar,
            IUIAutomationTreeWalker, TreeScope_Element, UIA_AutomationIdPropertyId,
            UIA_BoundingRectanglePropertyId, UIA_ControlTypePropertyId, UIA_DataItemControlTypeId,
            UIA_LegacyIAccessiblePatternId, UIA_ListItemControlTypeId, UIA_NamePropertyId,
            UIA_NativeWindowHandlePropertyId, UIA_PROPERTY_ID, UIAutomationPropertyInfo,
            UIAutomationType_Int,
        },
        UI::Shell::{IShellWindows, ItemIndex_Property_GUID},
    },
    core::{Error as WindowsError, w},
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
    item_walker: IUIAutomationTreeWalker,
    shell_windows: IShellWindows,
    active_folder_view: Option<shell::ActiveFolderView>,
    item_index_property: UIA_PROPERTY_ID,
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
        // SAFETY: The same live MTA owns this in-process, non-aggregated registrar interface.
        let registrar: IUIAutomationRegistrar =
            unsafe { CoCreateInstance(&CUIAutomationRegistrar, None, CLSCTX_INPROC_SERVER)? };
        // SAFETY: The registrar is live on its owning MTA and synchronously copies the fully
        // initialized property description and static programmatic-name string.
        let item_index_property = UIA_PROPERTY_ID(unsafe {
            registrar.RegisterProperty(&UIAutomationPropertyInfo {
                guid: ItemIndex_Property_GUID,
                pProgrammaticName: w!("ItemIndex"),
                r#type: UIAutomationType_Int,
            })?
        });
        if item_index_property.0 <= 0 {
            return Err(ResolverError::InvalidItemIndexProperty(
                item_index_property.0,
            ));
        }

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
        let (cache_request, control_walker, item_walker) = unsafe {
            let cache_request = automation.CreateCacheRequest()?;
            cache_request.SetTreeScope(TreeScope_Element)?;
            for property in [
                UIA_ControlTypePropertyId,
                UIA_BoundingRectanglePropertyId,
                UIA_NativeWindowHandlePropertyId,
                UIA_AutomationIdPropertyId,
                UIA_NamePropertyId,
                item_index_property,
            ] {
                cache_request.AddProperty(property)?;
            }
            cache_request.AddPattern(UIA_LegacyIAccessiblePatternId)?;

            let list_item = automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_ListItemControlTypeId.0),
            )?;
            let data_item = automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_DataItemControlTypeId.0),
            )?;
            let item_condition = automation.CreateOrCondition(&list_item, &data_item)?;

            (
                cache_request,
                automation.ControlViewWalker()?,
                automation.CreateTreeWalker(&item_condition)?,
            )
        };
        let shell_windows = shell::create_collection()?;

        Ok(Self {
            automation,
            cache_request,
            control_walker,
            item_walker,
            shell_windows,
            active_folder_view: None,
            item_index_property,
            last_trace: None,
            _apartment: apartment,
        })
    }

    fn select_active_view(
        &mut self,
        point: PhysicalScreenPoint,
        explorer_window: Option<cursorpeek_core::ExplorerWindowId>,
        evidence: candidate::CandidateEvidence<'_>,
    ) -> Result<shell::ActiveFolderView, shell::ShellRejection> {
        let first = shell::select(
            &self.shell_windows,
            &mut self.active_folder_view,
            point,
            explorer_window,
            evidence,
        );
        if !matches!(
            first,
            Err(shell::ShellRejection::ShellWindowsUnavailable(_))
        ) {
            return first;
        }

        self.shell_windows = shell::create_collection()
            .map_err(|error| shell::ShellRejection::ShellWindowsUnavailable(error.code().0))?;
        self.active_folder_view = None;
        shell::select(
            &self.shell_windows,
            &mut self.active_folder_view,
            point,
            explorer_window,
            evidence,
        )
    }

    fn inspect_point(&self, point: PhysicalScreenPoint) -> PointInspection {
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
                return PointInspection {
                    trace: ResolutionTrace::Rejected(RejectedTrace {
                        inspected: Vec::new(),
                        reason: RejectionReason::ElementLookupFailed(error.code().0),
                    }),
                    item_element: None,
                };
            }
        };

        let mut inspected = Vec::with_capacity(MAX_ANCESTORS + 1);
        let mut item_index = None;
        let mut item_element = None;

        for depth in 0..=MAX_ANCESTORS {
            let metadata = match self.read_cached_metadata(&element, depth) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return PointInspection {
                        trace: finish_trace(
                            inspected,
                            item_index,
                            WalkTermination::CachedMetadataFailed(error),
                        ),
                        item_element,
                    };
                }
            };

            if item_index.is_none() && metadata.control_kind.is_item() {
                if !metadata.bounds.is_ordered() {
                    let bounds = metadata.bounds;
                    inspected.push(metadata);
                    return PointInspection {
                        trace: ResolutionTrace::Rejected(RejectedTrace {
                            inspected,
                            reason: RejectionReason::InvalidItemBounds { depth, bounds },
                        }),
                        item_element: None,
                    };
                }
                if !metadata.bounds.contains(point) {
                    let bounds = metadata.bounds;
                    inspected.push(metadata);
                    return PointInspection {
                        trace: ResolutionTrace::Rejected(RejectedTrace {
                            inspected,
                            reason: RejectionReason::PointOutsideItemBounds {
                                depth,
                                bounds,
                                point,
                            },
                        }),
                        item_element: None,
                    };
                }
                item_index = Some(inspected.len());
                item_element = Some(element.clone());
            }
            inspected.push(metadata);

            if depth == MAX_ANCESTORS {
                return PointInspection {
                    trace: finish_trace(
                        inspected,
                        item_index,
                        WalkTermination::AncestorLimitReached,
                    ),
                    item_element,
                };
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
                    return PointInspection {
                        trace: finish_trace(
                            inspected,
                            item_index,
                            WalkTermination::ParentLookupFailed {
                                after_depth: depth,
                                code: error.code().0,
                            },
                        ),
                        item_element,
                    };
                }
            };
        }

        unreachable!("the bounded UI Automation walk always returns from the loop")
    }

    fn revalidate_candidate(
        &self,
        point: PhysicalScreenPoint,
        original_element: &IUIAutomationElement,
        original_evidence: &candidate::CandidateEvidence<'_>,
    ) -> Result<(), shell::ShellRejection> {
        // SAFETY: all interfaces remain on their owning MTA. ElementFromPoint retrieves the live
        // element at the same physical point; the conditioned walker then normalizes that hit to
        // the nearest ListItem/DataItem ancestor and refreshes only the existing bounded cache.
        // NormalizeElementBuildCache may return the root when no condition matches, so the cached
        // control kind and geometry are still checked below before any identity comparison.
        let hit = unsafe {
            self.automation.ElementFromPoint(POINT {
                x: point.x,
                y: point.y,
            })
        }
        .map_err(|error| shell::ShellRejection::CandidateRevalidationFailed(error.code().0))?;
        // SAFETY: `hit`, the walker, and cache request are live COM interfaces on their owning MTA;
        // the synchronous call returns an owned normalized element.
        let updated = unsafe {
            self.item_walker
                .NormalizeElementBuildCache(&hit, &self.cache_request)
        }
        .map_err(|error| shell::ShellRejection::CandidateRevalidationFailed(error.code().0))?;
        let metadata = self
            .read_cached_metadata(&updated, 0)
            .map_err(|error| shell::ShellRejection::CandidateRevalidationFailed(error.code))?;
        if !metadata.is_item_at(point) {
            return Err(shell::ShellRejection::CandidateChangedDuringVerification);
        }
        let updated_evidence = metadata
            .shell_evidence()
            .map_err(|_| shell::ShellRejection::CandidateChangedDuringVerification)?;

        // SAFETY: both elements and the automation client are live and apartment-local. The
        // comparison uses UIA runtime identity and returns a copied BOOL.
        let same_element = unsafe {
            self.automation
                .CompareElements(original_element, &updated)
                .map_err(|error| {
                    shell::ShellRejection::CandidateRevalidationFailed(error.code().0)
                })?
                .as_bool()
        };
        if !same_element || !original_evidence.same_fingerprint(&updated_evidence) {
            return Err(shell::ShellRejection::CandidateChangedDuringVerification);
        }
        Ok(())
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
            let name = element
                .CachedName()
                .map(|value| BoundedText::from_bstr(&value))
                .map_err(|error| cached_error(depth, CachedProperty::Name, error))?;
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
            let item_index = if control_kind.is_item() {
                let value = element
                    .GetCachedPropertyValue(self.item_index_property)
                    .map(OwnedVariant::new)
                    .map_err(|error| cached_error(depth, CachedProperty::ItemIndex, error))?;
                value.i32_value().filter(|index| *index != 0)
            } else {
                None
            };

            Ok(CachedElementMetadata {
                depth,
                control_kind,
                bounds,
                native_window,
                automation_id,
                name,
                has_legacy_pattern: legacy_pattern.is_some(),
                legacy_value,
                item_index,
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

    #[cfg(feature = "resolver-corpus")]
    pub(crate) fn observe(&mut self, point: PhysicalScreenPoint) -> CorpusObservation {
        let outcome = self.resolve(point, None);
        let trace = self
            .last_trace
            .as_ref()
            .expect("every completed resolution retains one bounded trace");
        let reason = trace.corpus_reason();
        let (status, path) = match outcome {
            ResolveOutcome::Resolved(target) => ("resolved", Some(target.path().to_path_buf())),
            ResolveOutcome::Unsupported => ("unsupported", None),
            ResolveOutcome::Ambiguous => ("ambiguous", None),
            ResolveOutcome::Unavailable => ("unavailable", None),
        };

        CorpusObservation {
            status,
            path,
            reason: reason.label,
            context_a: reason.context_a,
            context_b: reason.context_b,
        }
    }
}

impl PointResolver for ExplorerResolver {
    fn resolve(
        &mut self,
        point: PhysicalScreenPoint,
        explorer_window: Option<cursorpeek_core::ExplorerWindowId>,
    ) -> ResolveOutcome {
        let PointInspection {
            trace: uia,
            item_element,
        } = self.inspect_point(point);
        let (outcome, shell) = match &uia {
            ResolutionTrace::Rejected(_) => (
                ResolveOutcome::Unavailable,
                ShellStageTrace::NotAttemptedAfterUiaRejection,
            ),
            ResolutionTrace::Candidate(candidate) => match candidate.shell_evidence() {
                Ok(evidence) => {
                    let active_view = self.select_active_view(point, explorer_window, evidence);
                    let mut verification = match &active_view {
                        Ok(active_view) => shell::verify(active_view, point, evidence),
                        Err(reason) => shell::selection_failure(*reason),
                    };
                    if matches!(verification.outcome, ShellOutcome::Resolved(_)) {
                        let revalidation = item_element
                            .as_ref()
                            .ok_or(shell::ShellRejection::CandidateChangedDuringVerification)
                            .and_then(|element| {
                                self.revalidate_candidate(point, element, &evidence)
                            });
                        if let Err(reason) = revalidation {
                            verification = shell::ShellVerification {
                                outcome: ShellOutcome::Unavailable,
                                trace: ShellTrace::Rejected(reason),
                            };
                        }
                    }
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
                        CandidateEvidenceError::MissingItemsContainerAncestor => {
                            ResolveOutcome::Unsupported
                        }
                        CandidateEvidenceError::MissingItemIdentity
                        | CandidateEvidenceError::TruncatedLegacyValue
                        | CandidateEvidenceError::InvalidItemIndex(_) => {
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

struct PointInspection {
    trace: ResolutionTrace,
    item_element: Option<IUIAutomationElement>,
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

    #[cfg(feature = "resolver-corpus")]
    fn corpus_reason(&self) -> CorpusReason {
        match (&self.uia, self.shell) {
            (ResolutionTrace::Rejected(trace), _) => uia_rejection_reason(trace.reason),
            (
                ResolutionTrace::Candidate(_),
                ShellStageTrace::NotAttempted(
                    CandidateEvidenceError::MissingItemsContainerAncestor,
                ),
            ) => CorpusReason::new("uia.missing_items_container"),
            (
                ResolutionTrace::Candidate(_),
                ShellStageTrace::NotAttempted(CandidateEvidenceError::MissingItemIdentity),
            ) => CorpusReason::new("uia.missing_item_identity"),
            (
                ResolutionTrace::Candidate(_),
                ShellStageTrace::NotAttempted(CandidateEvidenceError::TruncatedLegacyValue),
            ) => CorpusReason::new("uia.truncated_legacy_value"),
            (
                ResolutionTrace::Candidate(_),
                ShellStageTrace::NotAttempted(CandidateEvidenceError::InvalidItemIndex(index)),
            ) => CorpusReason::with_context("uia.invalid_item_index", i64::from(index), 0),
            (
                ResolutionTrace::Candidate(_),
                ShellStageTrace::Attempted(ShellTrace::Resolved {
                    shell_windows,
                    view_items,
                }),
            ) => CorpusReason::with_context(
                "shell.resolved",
                i64::from(shell_windows),
                i64::from(view_items),
            ),
            (
                ResolutionTrace::Candidate(_),
                ShellStageTrace::Attempted(ShellTrace::Rejected(reason)),
            ) => shell_rejection_reason(reason),
            _ => unreachable!("the trace invariant excludes mismatched UIA and Shell stages"),
        }
    }
}

#[allow(dead_code)] // Commit 6 emits the bounded stage trace through the corpus runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellStageTrace {
    NotAttemptedAfterUiaRejection,
    NotAttempted(CandidateEvidenceError),
    Attempted(ShellTrace),
}

#[cfg(feature = "resolver-corpus")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CorpusReason {
    label: &'static str,
    context_a: i64,
    context_b: i64,
}

#[cfg(feature = "resolver-corpus")]
impl CorpusReason {
    const fn new(label: &'static str) -> Self {
        Self::with_context(label, 0, 0)
    }

    const fn with_context(label: &'static str, context_a: i64, context_b: i64) -> Self {
        Self {
            label,
            context_a,
            context_b,
        }
    }
}

#[cfg(feature = "resolver-corpus")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CorpusObservation {
    pub(crate) status: &'static str,
    pub(crate) path: Option<PathBuf>,
    pub(crate) reason: &'static str,
    pub(crate) context_a: i64,
    pub(crate) context_b: i64,
}

#[cfg(feature = "resolver-corpus")]
fn uia_rejection_reason(reason: RejectionReason) -> CorpusReason {
    match reason {
        RejectionReason::ElementLookupFailed(code) => {
            CorpusReason::with_context("uia.element_lookup_failed", i64::from(code), 0)
        }
        RejectionReason::InvalidItemBounds { depth, .. } => {
            CorpusReason::with_context("uia.invalid_item_bounds", depth as i64, 0)
        }
        RejectionReason::PointOutsideItemBounds { depth, .. } => {
            CorpusReason::with_context("uia.point_outside_item_bounds", depth as i64, 0)
        }
        RejectionReason::NoSupportedItem { termination } => walk_reason(termination),
    }
}

#[cfg(feature = "resolver-corpus")]
fn walk_reason(termination: WalkTermination) -> CorpusReason {
    match termination {
        WalkTermination::AncestorLimitReached => CorpusReason::new("uia.ancestor_limit"),
        WalkTermination::ParentLookupFailed { after_depth, code } => CorpusReason::with_context(
            "uia.parent_lookup_failed",
            after_depth as i64,
            i64::from(code),
        ),
        WalkTermination::CachedMetadataFailed(error) => CorpusReason::with_context(
            cached_property_reason(error.property),
            error.depth as i64,
            i64::from(error.code),
        ),
    }
}

#[cfg(feature = "resolver-corpus")]
const fn cached_property_reason(property: CachedProperty) -> &'static str {
    match property {
        CachedProperty::ControlType => "uia.cached_control_type_failed",
        CachedProperty::BoundingRectangle => "uia.cached_bounds_failed",
        CachedProperty::NativeWindowHandle => "uia.cached_native_window_failed",
        CachedProperty::AutomationId => "uia.cached_automation_id_failed",
        CachedProperty::Name => "uia.cached_name_failed",
        CachedProperty::ItemIndex => "uia.cached_item_index_failed",
    }
}

#[cfg(feature = "resolver-corpus")]
fn shell_rejection_reason(reason: shell::ShellRejection) -> CorpusReason {
    use shell::ShellRejection;

    match reason {
        ShellRejection::UnsupportedCandidatePath => {
            CorpusReason::new("shell.unsupported_candidate_path")
        }
        ShellRejection::UnsupportedResolvedPath => {
            CorpusReason::new("shell.unsupported_resolved_path")
        }
        ShellRejection::ShellWindowsUnavailable(code) => {
            CorpusReason::with_context("shell.shell_windows_unavailable", i64::from(code), 0)
        }
        ShellRejection::InvalidShellWindowCount(count) => {
            CorpusReason::with_context("shell.invalid_window_count", i64::from(count), 0)
        }
        ShellRejection::ShellWindowLimitExceeded(count) => {
            CorpusReason::with_context("shell.window_limit_exceeded", i64::from(count), 0)
        }
        ShellRejection::PointerWindowUnavailable => {
            CorpusReason::new("shell.pointer_window_unavailable")
        }
        ShellRejection::PointerLeftTargetExplorer => {
            CorpusReason::new("shell.pointer_left_target_explorer")
        }
        ShellRejection::ShellWindowItemFailed { index, code } => CorpusReason::with_context(
            "shell.window_item_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::BrowserServiceProviderFailed { index, code } => CorpusReason::with_context(
            "shell.browser_service_provider_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::TopLevelBrowserFailed { index, code } => CorpusReason::with_context(
            "shell.top_level_browser_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::ActiveShellViewFailed { index, code } => CorpusReason::with_context(
            "shell.active_view_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::ActiveViewIdentityFailed { index, code } => CorpusReason::with_context(
            "shell.active_view_identity_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::ActiveViewChanged => CorpusReason::new("shell.active_view_changed"),
        ShellRejection::FolderViewFailed { index, code } => CorpusReason::with_context(
            "shell.folder_view_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::NoActiveViewAtPoint { inspected } => {
            CorpusReason::with_context("shell.no_active_view_at_point", i64::from(inspected), 0)
        }
        ShellRejection::MultipleActiveViews => CorpusReason::new("shell.multiple_active_views"),
        ShellRejection::NativeWindowOutsideView => {
            CorpusReason::new("shell.native_window_outside_view")
        }
        ShellRejection::ViewItemsFailed(code) => {
            CorpusReason::with_context("shell.view_items_failed", i64::from(code), 0)
        }
        ShellRejection::InvalidViewItemCount(count) => {
            CorpusReason::with_context("shell.invalid_view_item_count", i64::from(count), 0)
        }
        ShellRejection::ViewItemLimitExceeded(count) => {
            CorpusReason::with_context("shell.view_item_limit_exceeded", i64::from(count), 0)
        }
        ShellRejection::CandidateItemIndexOutOfRange { index, count } => {
            CorpusReason::with_context(
                "shell.item_index_out_of_range",
                i64::from(index),
                i64::from(count),
            )
        }
        ShellRejection::CandidateIdentityMismatch { index } => {
            CorpusReason::with_context("shell.candidate_identity_mismatch", i64::from(index), 0)
        }
        ShellRejection::NoCandidateViewAtPoint { inspected } => {
            CorpusReason::with_context("shell.no_candidate_view_at_point", i64::from(inspected), 0)
        }
        ShellRejection::CandidateRevalidationFailed(code) => {
            CorpusReason::with_context("shell.candidate_revalidation_failed", i64::from(code), 0)
        }
        ShellRejection::CandidateChangedDuringVerification => {
            CorpusReason::new("shell.candidate_changed")
        }
        ShellRejection::InvalidTargetBounds => CorpusReason::new("shell.invalid_target_bounds"),
        ShellRejection::ViewItemFailed { index, code } => {
            CorpusReason::with_context("shell.view_item_failed", i64::from(index), i64::from(code))
        }
        ShellRejection::ViewItemPathFailed { index, code } => CorpusReason::with_context(
            "shell.view_item_path_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::ViewItemPathMalformed { index } => {
            CorpusReason::with_context("shell.view_item_path_malformed", i64::from(index), 0)
        }
        ShellRejection::ViewItemDisplayNameFailed { index, code } => CorpusReason::with_context(
            "shell.view_item_display_name_failed",
            i64::from(index),
            i64::from(code),
        ),
        ShellRejection::ViewItemDisplayNameMalformed { index } => CorpusReason::with_context(
            "shell.view_item_display_name_malformed",
            i64::from(index),
            0,
        ),
        ShellRejection::NoMatchingFilesystemItem { inspected } => {
            CorpusReason::with_context("shell.no_matching_filesystem_item", i64::from(inspected), 0)
        }
        ShellRejection::MultipleMatchingFilesystemItems => {
            CorpusReason::new("shell.multiple_matching_filesystem_items")
        }
        ShellRejection::MatchingItemAttributesFailed(code) => {
            CorpusReason::with_context("shell.matching_item_attributes_failed", i64::from(code), 0)
        }
        ShellRejection::MatchingItemIsNotAFile => {
            CorpusReason::new("shell.matching_item_is_not_a_file")
        }
    }
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

struct OwnedVariant(VARIANT);

impl OwnedVariant {
    fn new(value: VARIANT) -> Self {
        Self(value)
    }

    fn i32_value(&self) -> Option<i32> {
        // SAFETY: the active VARIANT arm is inspected only after checking its discriminant.
        unsafe {
            let value = &self.0.Anonymous.Anonymous;
            (value.vt == VT_I4).then(|| value.Anonymous.lVal)
        }
    }
}

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        // SAFETY: GetCachedPropertyValue initialized this VARIANT. This owner clears it exactly
        // once, including unsupported-property and wrong-type paths.
        let _ = unsafe { VariantClear(&mut self.0) };
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
    InvalidItemIndexProperty(i32),
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
            Self::InvalidItemIndexProperty(property) => {
                write!(
                    formatter,
                    "UI Automation returned invalid ItemIndex property ID {property}"
                )
            }
        }
    }
}

impl Error for ResolverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Windows(error) => Some(error),
            Self::TimeoutsNotApplied { .. } | Self::InvalidItemIndexProperty(_) => None,
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
    use windows::core::Interface;

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
            let shell_collection = Interface::as_raw(&resolver.shell_windows);
            assert!(matches!(
                resolver.resolve(point, None),
                ResolveOutcome::Unavailable
                    | ResolveOutcome::Unsupported
                    | ResolveOutcome::Ambiguous
            ));
            assert_eq!(
                Interface::as_raw(&resolver.shell_windows),
                shell_collection,
                "ordinary observations should reuse the apartment-local Shell collection"
            );
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
