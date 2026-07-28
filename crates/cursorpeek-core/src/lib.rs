#![deny(unsafe_code)]

mod types;

pub mod harness;
pub mod layout;
pub mod payload;
pub mod protocol;
pub mod sniff;

pub use types::{
    Generation, LegacyEncoding, PhysicalScreenPoint, PhysicalScreenRect, PhysicalScreenSpan,
};
