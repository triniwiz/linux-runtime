//! Web Workers: one thread, isolate and GLib context per worker.

use crate::event_loop::{begin_pending, post, post_state};
use crate::state::{state, State};
use crate::{throw_error, MetaSet, Runtime};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use v8::{ValueDeserializerHelper, ValueSerializerHelper};

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

/// Shared between a Worker object and its thread.
#[derive(Default)]
pub struct Link {
    /// worker's GMainContext once it's running, messages queue until then
    context: Mutex<Option<usize>>,
    pending: Mutex<Vec<Vec<u8>>>,
    isolate: Mutex<Option<v8::IsolateHandle>>,
    terminated: AtomicBool,
}

/// Worker side: where to post messages for the parent.
pub struct Parent {
    pub context: usize,
    pub id: u32,
}

struct Serializer;
impl v8::ValueSerializerImpl for Serializer {
    fn throw_data_clone_error<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        message: v8::Local<'s, v8::String>,
    ) {
        let error = v8::Exception::error(scope, message);
        scope.throw_exception(error);
    }
}

struct Deserializer;
impl v8::ValueDeserializerImpl for Deserializer {}

fn serialize<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Option<Vec<u8>> {
    let serializer = v8::ValueSerializer::new(scope, Box::new(Serializer));
    serializer.write_header();
    let context = scope.get_current_context();
    serializer.write_value(context, value)?;
    Some(serializer.release())
}

fn deserialize<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    data: &[u8],
) -> Option<v8::Local<'s, v8::Value>> {
    let deserializer = v8::ValueDeserializer::new(scope, Box::new(Deserializer), data);
    let context = scope.get_current_context();
    deserializer.read_header(context)?;
    deserializer.read_value(context)
}

fn dispatch<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    id: u32,
    kind: &str,
    value: v8::Local<'s, v8::Value>,
) {
    let Some(f) = st.worker_dispatch.borrow().clone() else {
        return;
    };
    let f = v8::Local::new(scope, f);
    let id = v8::Integer::new_from_unsigned(scope, id).into();
    let kind = v8::String::new(scope, kind).unwrap().into();
    let recv = v8::undefined(scope).into();
    f.call(scope, recv, &[id, kind, value]);
}

/// Delivers serialized `data` as a 'message' event to `id` (0 = the worker's global).
fn deliver(context: usize, id: u32, data: Vec<u8>) {
    post(
        context,
        false,
        Box::new(move |scope, st| match deserialize(scope, &data) {
            Some(v) => dispatch(scope, st, id, "message", v),
            None => {
                let e = v8::String::new(scope, "failed to deserialize message").unwrap();
                dispatch(scope, st, id, "messageerror", e.into());
            }
        }),
    );
}

fn run_worker(
    meta: Arc<MetaSet>,
    path: PathBuf,
    parent: Parent,
    link: Arc<Link>,
) -> Result<(), String> {
    let mut rt = Runtime::new_worker(meta, parent)?;
    let st = rt.state.clone();
    begin_pending(&st); // released by close() / terminate()
    *link.isolate.lock().unwrap() = Some(rt.isolate.thread_safe_handle());
    {
        let mut context = link.context.lock().unwrap();
        *context = Some(st.main_context.context);
        for data in link.pending.lock().unwrap().drain(..) {
            deliver(st.main_context.context, 0, data);
        }
    }
    let result = rt.run_file(&path).and_then(|_| rt.run_event_loop());
    *link.context.lock().unwrap() = None;
    link.isolate.lock().unwrap().take();
    result
}

/// workerSpawn(path) -> id
fn spawn<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let spec = args.get(0).to_rust_string_lossy(scope);
    let path = match spec.strip_prefix("file://") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_default().join(&spec),
    };
    if !path.is_file() {
        return throw_error(
            scope,
            &format!("Worker script not found: {}", path.display()),
        );
    }
    let id = st.next_request.get() + 1;
    st.next_request.set(id);
    let link = Arc::new(Link::default());
    st.workers.borrow_mut().insert(id, link.clone());
    begin_pending(&st); // released by the exit event
    let meta = st.meta.clone();
    let context = st.main_context.context;
    std::thread::Builder::new()
        .name(format!("worker-{id}"))
        .spawn(move || {
            let error = run_worker(meta, path, Parent { context, id }, link).err();
            post(
                context,
                true,
                Box::new(move |scope, st| {
                    st.workers.borrow_mut().remove(&id);
                    if let Some(e) = error.filter(|e| !e.is_empty()) {
                        let e = v8::String::new(scope, &e).unwrap();
                        dispatch(scope, st, id, "error", e.into());
                    }
                    let undefined = v8::undefined(scope).into();
                    dispatch(scope, st, id, "exit", undefined);
                }),
            );
        })
        .expect("failed to spawn worker thread");
    rv.set(v8::Integer::new_from_unsigned(scope, id).into());
}

/// workerPost(id, value): id 0 posts from a worker to its parent.
fn post_message<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let id = args.get(0).uint32_value(scope).unwrap_or(0);
    let Some(data) = serialize(scope, args.get(1)) else {
        return;
    };
    if id == 0 {
        if let Some(parent) = st.parent.borrow().as_ref() {
            deliver(parent.context, parent.id, data);
        }
        return;
    }
    let link = st.workers.borrow().get(&id).cloned();
    let Some(link) = link else { return };
    // hold the lock so the worker can't start draining in between
    let context = link.context.lock().unwrap();
    match *context {
        Some(context) => deliver(context, 0, data),
        None => link.pending.lock().unwrap().push(data),
    }
}

/// Stops the worker's event loop (and running script).
fn stop_worker(link: &Link) {
    if link.terminated.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(isolate) = link.isolate.lock().unwrap().as_ref() {
        isolate.terminate_execution();
    }
    if let Some(context) = *link.context.lock().unwrap() {
        post_state(
            context,
            Box::new(|st| {
                st.loop_refs.set(0);
                st.timers.borrow_mut().clear();
            }),
        );
    }
}

fn terminate<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let id = args.get(0).uint32_value(scope).unwrap_or(0);
    let link = st.workers.borrow().get(&id).cloned();
    if let Some(link) = link {
        stop_worker(&link);
    }
}

/// close() inside a worker
fn close<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    st.loop_refs.set(0);
    st.timers.borrow_mut().clear();
}

fn set_dispatch<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    if let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) {
        *st.worker_dispatch.borrow_mut() = Some(v8::Global::new(scope, f));
    }
}

pub fn install<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    natives: v8::Local<'s, v8::Object>,
) {
    macro_rules! set {
        ($name:literal, $f:expr) => {{
            let f = v8::Function::new(scope, $f).unwrap();
            let k = v8::String::new(scope, $name).unwrap();
            natives.set(scope, k.into(), f.into());
        }};
    }
    set!("workerSpawn", spawn);
    set!("workerPost", post_message);
    set!("workerTerminate", terminate);
    set!("workerClose", close);
    set!("workerSetDispatch", set_dispatch);
    let k = v8::String::new(scope, "isWorker").unwrap();
    let v = v8::Boolean::new(scope, st.parent.borrow().is_some());
    natives.set(scope, k.into(), v.into());
}

/// Terminates the workers of a runtime being dropped.
pub fn shutdown(st: &State) {
    for link in st.workers.borrow().values() {
        stop_worker(link);
    }
}
