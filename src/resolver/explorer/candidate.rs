use windows::{
    Win32::{
        Foundation::RECT,
        UI::Accessibility::{UIA_DataItemControlTypeId, UIA_ListItemControlTypeId},
    },
    core::BSTR,
};

use crate::hover::PhysicalScreenPoint;

pub(super) const MAX_ANCESTORS: usize = 8;
const MAX_CACHED_TEXT_UNITS: usize = 256;
const MAX_LEGACY_VALUE_UNITS: usize = 32_767;

pub(super) fn finish_trace(
    inspected: Vec<CachedElementMetadata>,
    item_index: Option<usize>,
    termination: WalkTermination,
) -> ResolutionTrace {
    match item_index {
        Some(item_index) => ResolutionTrace::Candidate(CandidateTrace {
            inspected,
            item_index,
            termination,
        }),
        None => ResolutionTrace::Rejected(RejectedTrace {
            inspected,
            reason: RejectionReason::NoSupportedItem { termination },
        }),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ResolutionTrace {
    Candidate(CandidateTrace),
    Rejected(RejectedTrace),
}

impl ResolutionTrace {
    pub(super) fn invariant_holds(&self, point: PhysicalScreenPoint) -> bool {
        match self {
            Self::Candidate(trace) => trace.invariant_holds(point),
            Self::Rejected(trace) => trace.invariant_holds(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CandidateTrace {
    inspected: Vec<CachedElementMetadata>,
    item_index: usize,
    termination: WalkTermination,
}

impl CandidateTrace {
    fn invariant_holds(&self, point: PhysicalScreenPoint) -> bool {
        let Some(item) = self.inspected.get(self.item_index) else {
            return false;
        };

        self.inspected.len() <= MAX_ANCESTORS + 1
            && self
                .inspected
                .iter()
                .enumerate()
                .all(|(depth, metadata)| metadata.invariant_holds(depth))
            && item.control_kind.is_item()
            && item.bounds.is_ordered()
            && item.bounds.contains(point)
            && self.termination.invariant_holds(self.inspected.len())
    }

    pub(super) fn shell_evidence(&self) -> Result<CandidateEvidence<'_>, CandidateEvidenceError> {
        let item = self
            .inspected
            .get(self.item_index)
            .expect("a valid candidate trace always retains its item");
        if !self
            .inspected
            .iter()
            .skip(self.item_index + 1)
            .any(|metadata| metadata.automation_id.equals_str("ItemsView"))
        {
            return Err(CandidateEvidenceError::MissingItemsViewAncestor);
        }

        let legacy_value = item
            .legacy_value
            .as_ref()
            .ok_or(CandidateEvidenceError::MissingLegacyValue)?;
        let path_units = legacy_value
            .complete_units()
            .ok_or(CandidateEvidenceError::TruncatedLegacyValue)?;

        Ok(CandidateEvidence {
            path_units,
            item_native_window: item.native_window,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CandidateEvidence<'a> {
    pub(super) path_units: &'a [u16],
    pub(super) item_native_window: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateEvidenceError {
    MissingItemsViewAncestor,
    MissingLegacyValue,
    TruncatedLegacyValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RejectedTrace {
    pub(super) inspected: Vec<CachedElementMetadata>,
    pub(super) reason: RejectionReason,
}

impl RejectedTrace {
    fn invariant_holds(&self) -> bool {
        self.inspected.len() <= MAX_ANCESTORS + 1
            && self
                .inspected
                .iter()
                .enumerate()
                .all(|(depth, metadata)| metadata.invariant_holds(depth))
            && self.reason.invariant_holds(self.inspected.len())
    }
}

#[allow(dead_code)] // The complete trace becomes corpus output in the next resolver checkpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RejectionReason {
    ElementLookupFailed(i32),
    InvalidItemBounds {
        depth: usize,
        bounds: CachedRect,
    },
    PointOutsideItemBounds {
        depth: usize,
        bounds: CachedRect,
        point: PhysicalScreenPoint,
    },
    NoSupportedItem {
        termination: WalkTermination,
    },
}

impl RejectionReason {
    fn invariant_holds(self, inspected_len: usize) -> bool {
        match self {
            Self::ElementLookupFailed(_) => inspected_len == 0,
            Self::InvalidItemBounds { depth, bounds } => {
                depth + 1 == inspected_len && !bounds.is_ordered()
            }
            Self::PointOutsideItemBounds {
                depth,
                bounds,
                point,
            } => depth + 1 == inspected_len && bounds.is_ordered() && !bounds.contains(point),
            Self::NoSupportedItem { termination } => termination.invariant_holds(inspected_len),
        }
    }
}

#[allow(dead_code)] // HRESULT context is retained for the forthcoming resolver corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WalkTermination {
    AncestorLimitReached,
    ParentLookupFailed { after_depth: usize, code: i32 },
    CachedMetadataFailed(CachedMetadataError),
}

impl WalkTermination {
    fn invariant_holds(self, inspected_len: usize) -> bool {
        match self {
            Self::AncestorLimitReached => inspected_len == MAX_ANCESTORS + 1,
            Self::ParentLookupFailed { after_depth, .. } => after_depth + 1 == inspected_len,
            Self::CachedMetadataFailed(error) => error.depth == inspected_len,
        }
    }
}

#[allow(dead_code)] // Property and HRESULT identify provider failures in diagnostic traces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CachedMetadataError {
    pub(super) depth: usize,
    pub(super) property: CachedProperty,
    pub(super) code: i32,
}

#[allow(dead_code)] // Each property label is preserved for structured diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CachedProperty {
    ControlType,
    Name,
    BoundingRectangle,
    NativeWindowHandle,
    AutomationId,
}

#[allow(dead_code)] // Shell correlation consumes the HWND/pattern evidence in the next checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CachedElementMetadata {
    pub(super) depth: usize,
    pub(super) control_kind: ControlKind,
    pub(super) name: BoundedText,
    pub(super) bounds: CachedRect,
    pub(super) native_window: usize,
    pub(super) automation_id: BoundedText,
    pub(super) has_legacy_pattern: bool,
    pub(super) legacy_value: Option<BoundedLegacyValue>,
}

impl CachedElementMetadata {
    fn invariant_holds(&self, expected_depth: usize) -> bool {
        self.depth == expected_depth
            && self.name.invariant_holds()
            && self.automation_id.invariant_holds()
            && self
                .legacy_value
                .as_ref()
                .is_none_or(BoundedLegacyValue::invariant_holds)
            && match self.control_kind {
                ControlKind::ListItem | ControlKind::DataItem | ControlKind::Other(_) => true,
            }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ControlKind {
    ListItem,
    DataItem,
    Other(i32),
}

impl ControlKind {
    pub(super) fn from_raw(value: i32) -> Self {
        if value == UIA_ListItemControlTypeId.0 {
            Self::ListItem
        } else if value == UIA_DataItemControlTypeId.0 {
            Self::DataItem
        } else {
            Self::Other(value)
        }
    }

    pub(super) fn is_item(self) -> bool {
        matches!(self, Self::ListItem | Self::DataItem)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CachedRect {
    pub(super) left: i32,
    pub(super) top: i32,
    pub(super) right: i32,
    pub(super) bottom: i32,
}

impl CachedRect {
    pub(super) fn is_ordered(self) -> bool {
        self.left < self.right && self.top < self.bottom
    }

    pub(super) fn contains(self, point: PhysicalScreenPoint) -> bool {
        self.left <= point.x && point.x < self.right && self.top <= point.y && point.y < self.bottom
    }
}

impl From<RECT> for CachedRect {
    fn from(value: RECT) -> Self {
        Self {
            left: value.left,
            top: value.top,
            right: value.right,
            bottom: value.bottom,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BoundedText {
    units: Box<[u16]>,
    source_units: usize,
}

impl BoundedText {
    pub(super) fn from_bstr(value: &BSTR) -> Self {
        Self::from_units(value)
    }

    fn from_units(value: &[u16]) -> Self {
        let mut end = value.len().min(MAX_CACHED_TEXT_UNITS);
        if end < value.len()
            && end > 0
            && is_high_surrogate(value[end - 1])
            && is_low_surrogate(value[end])
        {
            end -= 1;
        }

        Self {
            units: value[..end].into(),
            source_units: value.len(),
        }
    }

    fn was_truncated(&self) -> bool {
        self.units.len() < self.source_units
    }

    fn invariant_holds(&self) -> bool {
        self.units.len() <= MAX_CACHED_TEXT_UNITS
            && self.units.len() <= self.source_units
            && (!self.was_truncated() || self.source_units > self.units.len())
    }

    fn equals_str(&self, expected: &str) -> bool {
        !self.was_truncated() && self.units.iter().copied().eq(expected.encode_utf16())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BoundedLegacyValue {
    units: Box<[u16]>,
    source_units: usize,
}

impl BoundedLegacyValue {
    pub(super) fn from_bstr(value: &BSTR) -> Self {
        let mut end = value.len().min(MAX_LEGACY_VALUE_UNITS);
        if end < value.len()
            && end > 0
            && is_high_surrogate(value[end - 1])
            && is_low_surrogate(value[end])
        {
            end -= 1;
        }

        Self {
            units: value[..end].into(),
            source_units: value.len(),
        }
    }

    fn complete_units(&self) -> Option<&[u16]> {
        (self.units.len() == self.source_units).then_some(&self.units)
    }

    fn invariant_holds(&self) -> bool {
        self.units.len() <= MAX_LEGACY_VALUE_UNITS && self.units.len() <= self.source_units
    }
}

fn is_high_surrogate(value: u16) -> bool {
    (0xd800..=0xdbff).contains(&value)
}

fn is_low_surrogate(value: u16) -> bool {
    (0xdc00..=0xdfff).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::{
        BoundedLegacyValue, BoundedText, CachedElementMetadata, CachedRect, CandidateEvidenceError,
        CandidateTrace, ControlKind, MAX_CACHED_TEXT_UNITS, MAX_LEGACY_VALUE_UNITS,
        ResolutionTrace, WalkTermination,
    };
    use crate::hover::PhysicalScreenPoint;
    use windows::Win32::UI::Accessibility::{
        UIA_ButtonControlTypeId, UIA_DataItemControlTypeId, UIA_ListItemControlTypeId,
    };

    #[test]
    fn control_classification_accepts_only_explorer_item_shapes() {
        assert_eq!(
            ControlKind::from_raw(UIA_ListItemControlTypeId.0),
            ControlKind::ListItem
        );
        assert_eq!(
            ControlKind::from_raw(UIA_DataItemControlTypeId.0),
            ControlKind::DataItem
        );
        assert_eq!(
            ControlKind::from_raw(UIA_ButtonControlTypeId.0),
            ControlKind::Other(UIA_ButtonControlTypeId.0)
        );
    }

    #[test]
    fn cached_rectangles_are_nonempty_and_half_open() {
        let bounds = CachedRect {
            left: -10,
            top: -20,
            right: 10,
            bottom: 20,
        };
        assert!(bounds.is_ordered());
        assert!(bounds.contains(PhysicalScreenPoint::new(-10, -20)));
        assert!(bounds.contains(PhysicalScreenPoint::new(9, 19)));
        assert!(!bounds.contains(PhysicalScreenPoint::new(10, 0)));
        assert!(!bounds.contains(PhysicalScreenPoint::new(0, 20)));

        assert!(
            !CachedRect {
                left: 4,
                top: 0,
                right: 4,
                bottom: 1,
            }
            .is_ordered()
        );
    }

    #[test]
    fn cached_text_is_bounded_without_splitting_a_surrogate_pair() {
        let mut source = vec![u16::from(b'a'); MAX_CACHED_TEXT_UNITS - 1];
        source.extend([0xd83d, 0xde00]);

        let text = BoundedText::from_units(&source);

        assert_eq!(text.units.len(), MAX_CACHED_TEXT_UNITS - 1);
        assert_eq!(text.source_units, MAX_CACHED_TEXT_UNITS + 1);
        assert!(text.was_truncated());
        assert!(text.invariant_holds());
    }

    #[test]
    fn short_cached_text_is_preserved_exactly() {
        let source = "ItemsView".encode_utf16().collect::<Vec<_>>();
        let text = BoundedText::from_units(&source);

        assert_eq!(text.units.as_ref(), source);
        assert_eq!(text.source_units, source.len());
        assert!(!text.was_truncated());
        assert!(text.invariant_holds());
    }

    fn metadata(
        depth: usize,
        control_kind: ControlKind,
        automation_id: &str,
        legacy_value: Option<&str>,
    ) -> CachedElementMetadata {
        CachedElementMetadata {
            depth,
            control_kind,
            name: BoundedText::from_units(&[]),
            bounds: CachedRect {
                left: 0,
                top: 0,
                right: 10,
                bottom: 10,
            },
            native_window: 42,
            automation_id: BoundedText::from_units(
                &automation_id.encode_utf16().collect::<Vec<_>>(),
            ),
            has_legacy_pattern: legacy_value.is_some(),
            legacy_value: legacy_value
                .map(|value| BoundedLegacyValue::from_bstr(&windows::core::BSTR::from(value))),
        }
    }

    #[test]
    fn shell_evidence_requires_items_view_ancestry_and_complete_legacy_value() {
        let candidate = CandidateTrace {
            inspected: vec![
                metadata(0, ControlKind::ListItem, "", Some(r"C:\preview.txt")),
                metadata(1, ControlKind::Other(0), "ItemsView", None),
            ],
            item_index: 0,
            termination: WalkTermination::ParentLookupFailed {
                after_depth: 1,
                code: 0,
            },
        };
        let trace = ResolutionTrace::Candidate(candidate.clone());
        let ResolutionTrace::Candidate(candidate) = trace else {
            unreachable!();
        };
        let evidence = candidate.shell_evidence().unwrap();
        assert_eq!(
            evidence.path_units,
            r"C:\preview.txt".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(evidence.item_native_window, 42);

        let mut no_items_view = candidate.clone();
        no_items_view.inspected[1].automation_id = BoundedText::from_units(&[]);
        assert_eq!(
            no_items_view.shell_evidence(),
            Err(CandidateEvidenceError::MissingItemsViewAncestor)
        );

        let mut no_value = candidate;
        no_value.inspected[0].legacy_value = None;
        assert_eq!(
            no_value.shell_evidence(),
            Err(CandidateEvidenceError::MissingLegacyValue)
        );
    }

    #[test]
    fn oversized_legacy_values_cannot_become_shell_path_evidence() {
        let value =
            windows::core::BSTR::from_wide(&vec![u16::from(b'a'); MAX_LEGACY_VALUE_UNITS + 1]);
        let mut candidate = CandidateTrace {
            inspected: vec![
                metadata(0, ControlKind::DataItem, "", None),
                metadata(1, ControlKind::Other(0), "ItemsView", None),
            ],
            item_index: 0,
            termination: WalkTermination::ParentLookupFailed {
                after_depth: 1,
                code: 0,
            },
        };
        candidate.inspected[0].legacy_value = Some(BoundedLegacyValue::from_bstr(&value));

        assert_eq!(
            candidate.shell_evidence(),
            Err(CandidateEvidenceError::TruncatedLegacyValue)
        );
    }
}
