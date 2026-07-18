use windows::{
    Win32::{
        Foundation::RECT,
        UI::Accessibility::{
            UIA_DataItemControlTypeId, UIA_ListControlTypeId, UIA_ListItemControlTypeId,
        },
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
            && item.is_item_at(point)
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
            .any(|metadata| {
                metadata.control_kind.is_items_container()
                    || metadata.automation_id.equals_str("ItemsView")
            })
        {
            return Err(CandidateEvidenceError::MissingItemsContainerAncestor);
        }

        item.shell_evidence()
    }
}

impl CachedElementMetadata {
    pub(super) fn is_item_at(&self, point: PhysicalScreenPoint) -> bool {
        self.control_kind.is_item() && self.bounds.is_ordered() && self.bounds.contains(point)
    }

    pub(super) fn shell_evidence(&self) -> Result<CandidateEvidence<'_>, CandidateEvidenceError> {
        debug_assert!(self.control_kind.is_item());
        let view_index = match self.item_index {
            Some(index) if index > 0 => Some(
                u32::try_from(index - 1)
                    .expect("a positive UI Automation item index fits a zero-based u32 index"),
            ),
            Some(0) | None => None,
            Some(index) => return Err(CandidateEvidenceError::InvalidItemIndex(index)),
        };
        let path_units = match self.legacy_value.as_ref() {
            Some(value) => match value.complete_units() {
                Some(units) => Some(units),
                None if view_index.is_some() => None,
                None => return Err(CandidateEvidenceError::TruncatedLegacyValue),
            },
            None => None,
        };
        if path_units.is_none() && view_index.is_none() {
            return Err(CandidateEvidenceError::MissingItemIdentity);
        }

        Ok(CandidateEvidence {
            path_units,
            view_index,
            item_native_window: self.native_window,
            item_bounds: self.bounds,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CandidateEvidence<'a> {
    pub(super) path_units: Option<&'a [u16]>,
    pub(super) view_index: Option<u32>,
    pub(super) item_native_window: usize,
    pub(super) item_bounds: CachedRect,
}

impl CandidateEvidence<'_> {
    pub(super) fn same_fingerprint(&self, other: &CandidateEvidence<'_>) -> bool {
        self.path_units == other.path_units
            && self.view_index == other.view_index
            && self.item_native_window == other.item_native_window
            && self.item_bounds == other.item_bounds
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateEvidenceError {
    MissingItemsContainerAncestor,
    MissingItemIdentity,
    TruncatedLegacyValue,
    InvalidItemIndex(i32),
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
    BoundingRectangle,
    NativeWindowHandle,
    AutomationId,
    ItemIndex,
}

#[allow(dead_code)] // Shell correlation consumes the HWND/pattern evidence in the next checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CachedElementMetadata {
    pub(super) depth: usize,
    pub(super) control_kind: ControlKind,
    pub(super) bounds: CachedRect,
    pub(super) native_window: usize,
    pub(super) automation_id: BoundedText,
    pub(super) has_legacy_pattern: bool,
    pub(super) legacy_value: Option<BoundedLegacyValue>,
    pub(super) item_index: Option<i32>,
}

impl CachedElementMetadata {
    fn invariant_holds(&self, expected_depth: usize) -> bool {
        self.depth == expected_depth
            && self.automation_id.invariant_holds()
            && self
                .legacy_value
                .as_ref()
                .is_none_or(BoundedLegacyValue::invariant_holds)
            && match self.control_kind {
                ControlKind::List
                | ControlKind::ListItem
                | ControlKind::DataItem
                | ControlKind::Other(_) => true,
            }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ControlKind {
    List,
    ListItem,
    DataItem,
    Other(i32),
}

impl ControlKind {
    pub(super) fn from_raw(value: i32) -> Self {
        if value == UIA_ListControlTypeId.0 {
            Self::List
        } else if value == UIA_ListItemControlTypeId.0 {
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

    fn is_items_container(self) -> bool {
        self == Self::List
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
        UIA_ButtonControlTypeId, UIA_DataItemControlTypeId, UIA_ListControlTypeId,
        UIA_ListItemControlTypeId,
    };

    #[test]
    fn control_classification_accepts_only_explorer_item_shapes() {
        assert_eq!(
            ControlKind::from_raw(UIA_ListControlTypeId.0),
            ControlKind::List
        );
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
            item_index: None,
        }
    }

    #[test]
    fn shell_evidence_requires_an_items_container_and_one_complete_identity() {
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
            evidence.path_units.unwrap(),
            r"C:\preview.txt".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(evidence.view_index, None);
        assert_eq!(evidence.item_native_window, 42);

        let mut list_container = candidate.clone();
        list_container.inspected[1].automation_id = BoundedText::from_units(&[]);
        list_container.inspected[1].control_kind = ControlKind::List;
        assert!(list_container.shell_evidence().is_ok());

        let mut no_items_container = candidate.clone();
        no_items_container.inspected[1].automation_id = BoundedText::from_units(&[]);
        assert_eq!(
            no_items_container.shell_evidence(),
            Err(CandidateEvidenceError::MissingItemsContainerAncestor)
        );

        let mut no_value = candidate;
        no_value.inspected[0].legacy_value = None;
        assert_eq!(
            no_value.shell_evidence(),
            Err(CandidateEvidenceError::MissingItemIdentity)
        );

        no_value.inspected[0].item_index = Some(0);
        assert_eq!(
            no_value.shell_evidence(),
            Err(CandidateEvidenceError::MissingItemIdentity)
        );

        no_value.inspected[0].item_index = Some(1);
        let index_evidence = no_value.shell_evidence().unwrap();
        assert_eq!(index_evidence.path_units, None);
        assert_eq!(index_evidence.view_index, Some(0));

        no_value.inspected[0].item_index = Some(-1);
        assert_eq!(
            no_value.shell_evidence(),
            Err(CandidateEvidenceError::InvalidItemIndex(-1))
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

        candidate.inspected[0].item_index = Some(2);
        let evidence = candidate.shell_evidence().unwrap();
        assert_eq!(evidence.path_units, None);
        assert_eq!(evidence.view_index, Some(1));
    }

    #[test]
    fn candidate_fingerprint_detects_identity_window_and_geometry_changes() {
        let candidate = CandidateTrace {
            inspected: vec![
                metadata(0, ControlKind::ListItem, "", Some(r"C:\preview.txt")),
                metadata(1, ControlKind::List, "", None),
            ],
            item_index: 0,
            termination: WalkTermination::ParentLookupFailed {
                after_depth: 1,
                code: 0,
            },
        };
        let original = candidate.shell_evidence().unwrap();
        assert!(original.same_fingerprint(&original));

        let mut changed = candidate.clone();
        changed.inspected[0].item_index = Some(2);
        let changed_index = changed.shell_evidence().unwrap();
        assert!(!original.same_fingerprint(&changed_index));

        changed.inspected[0].item_index = None;
        changed.inspected[0].bounds.right += 1;
        let changed_bounds = changed.shell_evidence().unwrap();
        assert!(!original.same_fingerprint(&changed_bounds));

        changed.inspected[0].bounds = candidate.inspected[0].bounds;
        changed.inspected[0].native_window += 1;
        let changed_window = changed.shell_evidence().unwrap();
        assert!(!original.same_fingerprint(&changed_window));
    }

    #[test]
    fn refreshed_item_requires_an_item_shape_containing_the_point() {
        let mut item = metadata(0, ControlKind::ListItem, "", Some(r"C:\preview.txt"));
        assert!(item.is_item_at(PhysicalScreenPoint::new(5, 5)));
        assert!(!item.is_item_at(PhysicalScreenPoint::new(10, 5)));

        item.control_kind = ControlKind::Other(0);
        assert!(!item.is_item_at(PhysicalScreenPoint::new(5, 5)));

        item.control_kind = ControlKind::DataItem;
        item.bounds.right = item.bounds.left;
        assert!(!item.is_item_at(PhysicalScreenPoint::new(5, 5)));
    }
}
