mod explorer;

use std::path::{Path, PathBuf};

use crate::hover::PhysicalScreenPoint;
use cursorpeek_core::{ExplorerWindowId, PhysicalScreenRect};

pub(crate) use explorer::{ExplorerResolver, ResolverError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolveOutcome {
    Resolved(ResolvedTarget),
    Unsupported,
    Ambiguous,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedTarget {
    path: PathBuf,
    target_bounds: PhysicalScreenRect,
}

impl ResolvedTarget {
    pub(crate) fn new(path: PathBuf, target_bounds: PhysicalScreenRect) -> Self {
        Self {
            path,
            target_bounds,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn target_bounds(&self) -> PhysicalScreenRect {
        self.target_bounds
    }
}

pub(crate) trait PointResolver {
    fn resolve(
        &mut self,
        point: PhysicalScreenPoint,
        explorer_window: Option<ExplorerWindowId>,
    ) -> ResolveOutcome;
}
