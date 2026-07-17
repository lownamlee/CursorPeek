mod explorer;

use std::path::{Path, PathBuf};

use crate::hover::PhysicalScreenPoint;

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
}

impl ResolvedTarget {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) trait PointResolver {
    fn resolve(&mut self, point: PhysicalScreenPoint) -> ResolveOutcome;
}
