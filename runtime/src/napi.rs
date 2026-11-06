//! Node-API addons.

use crate::state::State;
use std::path::Path;

pub fn load_addon<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    _st: &State,
    path: &Path,
) -> Result<v8::Local<'s, v8::Value>, Option<String>> {
    Err(Some(format!("{}: Node-API addons aren't supported yet", path.display())))
}
