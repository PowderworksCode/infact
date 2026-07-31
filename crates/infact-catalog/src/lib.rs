//! Build compact external API catalogs from rustdoc JSON.

mod rustdoc;

pub use rustdoc::{CatalogRequest, Error, Result, build_catalog};
