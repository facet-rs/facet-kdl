#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

// cf. facet-toml/facet-json for examples

mod deserialize;
pub use deserialize::*;

mod serialize;
pub use serialize::*;
