// napi-android's js_* host API over V8, for its v8-api.cpp shim (replaces the
// Android jsr.cpp). The platform and V8 itself are initialized from Rust
// (rusty_v8): its NewDefaultPlatform returns a libc++ std::unique_ptr this
// libstdc++ build can't take.

#include "v8-api.h"
#include "js_native_api.h"
#include "SimpleAllocator.h"

struct napi_runtime__ {
    tns::SimpleAllocator *allocator;
    v8::Isolate *isolate;
};

extern "C" void ns_v8_fatal_error(const char *location, const char *message);
extern "C" void ns_v8_pump(v8::Isolate *isolate);

static void OnFatalError(const char *location, const char *message) {
    ns_v8_fatal_error(location, message);
}

static void OnOOMError(const char *location, const v8::OOMDetails &details) {
    ns_v8_fatal_error(location, details.detail ? details.detail : "out of memory");
}

// Feeds the host's `onUnhandledPromiseRejectionTracker(promise, reason, handled)`
// (napi-android's convention, as the QuickJS shim does).
static void OnPromiseReject(v8::PromiseRejectMessage message) {
    auto event = message.GetEvent();
    if (event != v8::kPromiseRejectWithNoHandler && event != v8::kPromiseHandlerAddedAfterReject) return;
    v8::Local<v8::Promise> promise = message.GetPromise();
    v8::Isolate *isolate = v8::Isolate::GetCurrent();
    v8::HandleScope scope(isolate);
    v8::Local<v8::Context> context = isolate->GetCurrentContext();
    v8::Local<v8::Value> tracker;
    auto name = v8::String::NewFromUtf8Literal(isolate, "onUnhandledPromiseRejectionTracker");
    if (!context->Global()->Get(context, name).ToLocal(&tracker) || !tracker->IsFunction()) return;
    bool handled = event == v8::kPromiseHandlerAddedAfterReject;
    v8::Local<v8::Value> reason = handled ? v8::Undefined(isolate).As<v8::Value>() : message.GetValue();
    v8::Local<v8::Value> args[] = {promise, reason, v8::Boolean::New(isolate, handled)};
    v8::TryCatch tc(isolate);
    (void)tracker.As<v8::Function>()->Call(context, v8::Undefined(isolate), 3, args);
}

extern "C" {

napi_status js_create_runtime(napi_runtime__ **runtime) {
    if (!runtime) return napi_invalid_arg;
    auto *rt = new napi_runtime__();
    rt->allocator = new tns::SimpleAllocator();
    v8::Isolate::CreateParams params;
    params.array_buffer_allocator = rt->allocator;
    rt->isolate = v8::Isolate::New(params);
    rt->isolate->SetFatalErrorHandler(&OnFatalError);
    rt->isolate->SetOOMErrorHandler(&OnOOMError);
    // checkpoints come from js_execute_pending_jobs
    rt->isolate->SetMicrotasksPolicy(v8::MicrotasksPolicy::kExplicit);
    rt->isolate->SetPromiseRejectCallback(&OnPromiseReject);
    rt->isolate->SetCaptureStackTraceForUncaughtExceptions(true, 50);
    *runtime = rt;
    return napi_ok;
}

// Enters the isolate and a context for the life of the env, with a napi
// handle scope open so the host can make handles outside callbacks.
napi_status js_create_napi_env(napi_env *env, napi_runtime__ *rt) {
    if (!env || !rt) return napi_invalid_arg;
    v8::Isolate *isolate = rt->isolate;
    isolate->Enter();
    {
        v8::HandleScope handle_scope(isolate);
        v8::Local<v8::Context> context = v8::Context::New(isolate);
        context->Enter();
        *env = new napi_env__(context, NAPI_VERSION_EXPERIMENTAL);
    }
    return napi_ok;
}

napi_status js_free_napi_env(napi_env env) {
    if (!env) return napi_invalid_arg;
    v8::Isolate *isolate = env->isolate;
    {
        v8::HandleScope scope(isolate);
        env->context()->Exit();
    }
    env->DeleteMe();
    isolate->Exit();
    return napi_ok;
}

napi_status js_free_runtime(napi_runtime__ *rt) {
    if (!rt) return napi_invalid_arg;
    rt->isolate->Dispose();
    delete rt->allocator;
    delete rt;
    return napi_ok;
}

napi_status js_execute_script(napi_env env, napi_value script, const char *file,
                              napi_value *result) {
    return napi_run_script_source(env, script, file, result);
}

// Platform tasks, then microtasks; a throw is left pending for the host.
napi_status js_execute_pending_jobs(napi_env env) {
    v8::Isolate *isolate = env->isolate;
    ns_v8_pump(isolate);
    v8::HandleScope scope(isolate);
    v8::TryCatch tc(isolate);
    isolate->PerformMicrotaskCheckpoint();
    if (tc.HasCaught() && !tc.HasTerminated()) {
        env->last_exception.Reset(isolate, tc.Exception());
        return napi_pending_exception;
    }
    return napi_ok;
}

napi_status js_get_runtime_version(napi_env env, napi_value *version) {
    return napi_create_string_utf8(env, "V8", NAPI_AUTO_LENGTH, version);
}

v8::Isolate *js_isolate(napi_env env) {
    return env ? env->isolate : nullptr;
}

} // extern "C"
