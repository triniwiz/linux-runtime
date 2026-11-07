//! Engine independent core shared by the V8 and Node-API runtimes.

pub mod base64;
pub mod bundle;
pub mod call;
pub mod closure;
pub mod glib;
pub mod longdouble;
pub mod native;
pub mod resolve;

pub use native::{align_up, dlerror, set_library_dirs, Field, Layout, Native, RecordLayout};
pub use nativescript_metadata as metadata;
