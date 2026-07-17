mod explorer;

use crate::hover::PhysicalScreenPoint;

pub(crate) use explorer::{ExplorerResolver, ResolverError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolveOutcome {
    Unavailable,
}

pub(crate) trait PointResolver {
    fn resolve(&mut self, point: PhysicalScreenPoint) -> ResolveOutcome;
}
