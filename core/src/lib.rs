//! Engine independent core shared by the V8 and Node-API runtimes.

pub mod call;
pub mod closure;
pub mod glib;
pub mod longdouble;
pub mod native;
pub mod resolve;

pub use nativescript_metadata as metadata;
pub use native::{align_up, dlerror, Field, Layout, Native, RecordLayout};
