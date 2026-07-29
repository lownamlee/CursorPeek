#![deny(unsafe_code)]

mod types;

pub mod harness;
pub mod layout;
pub mod payload;
pub mod protocol;
pub mod sniff;
pub mod svg;

pub use types::{
    ExplorerWindowId, Generation, LegacyEncoding, PhysicalScreenPoint, PhysicalScreenRect,
    PhysicalScreenSpan,
};
