#include "jsr.h"
#include <cstring> // [linux port] strlen
#include <dlfcn.h>

// Collects now; JSGarbageCollect only hints that a collection would help.
extern "C" JS_EXPORT void JSSynchronousGarbageCollectForDebugging(JSContextRef ctx);
// [linux port] JSContextRefPrivate.h: called with (promise, reason) once microtasks drain
extern "C" JS_EXPORT void JSGlobalContextSetUnhandledRejectionCallback(JSGlobalContextRef ctx, JSObjectRef function, JSValueRef* exception);

napi_status js_create_runtime(napi_runtime *runtime) {
    if (!runtime) return napi_invalid_arg;
    // [linux port] a classed global gets an ordinary object as its prototype, whose
    // [[Prototype]] can change (the global's own is immutable)
    static JSClassRef global_class = JSClassCreate(&kJSClassDefinitionEmpty);
    *runtime = (napi_runtime) JSGlobalContextCreateInGroup(nullptr, global_class);
    return napi_ok;
}

napi_status js_create_napi_env(napi_env* env, napi_runtime runtime) {
    if (env == nullptr) return napi_invalid_arg;

    *env = new napi_env__((JSGlobalContextRef) runtime);
    JSGlobalContextRelease((JSGlobalContextRef) runtime);

    napi_value gc;
    napi_create_function(*env, "gc", strlen("gc"), [](napi_env env, napi_callback_info info) -> napi_value {
        JSSynchronousGarbageCollectForDebugging(env->context);
        napi_value undefined;
        napi_get_undefined(env, &undefined);
        return undefined;
    }, nullptr, &gc);
    napi_value global;
    napi_get_global(*env, &global);
    napi_set_named_property(*env, global, "gc", gc);

    // [linux port] feeds the host's onUnhandledPromiseRejectionTracker (the QuickJS shim's convention)
    JSStringRef tracker = JSStringCreateWithUTF8CString(
        "(function (p, r) { var t = globalThis.onUnhandledPromiseRejectionTracker; if (typeof t === 'function') t(p, r, false); })");
    JSValueRef fn = JSEvaluateScript((*env)->context, tracker, nullptr, nullptr, 0, nullptr);
    JSStringRelease(tracker);
    if (fn && JSValueIsObject((*env)->context, fn))
        JSGlobalContextSetUnhandledRejectionCallback((*env)->context, (JSObjectRef) fn, nullptr);

    return napi_ok;

}

napi_status js_set_runtime_flags(const char* flags) {
    return napi_ok;
}

napi_status js_lock_env(napi_env env) {
    return napi_ok;
}

napi_status js_unlock_env(napi_env env) {
    return napi_ok;
}

napi_status js_free_napi_env(napi_env env) {
    if (env == nullptr) return napi_invalid_arg;
    delete env;
    return  napi_ok;
}

napi_status js_free_runtime(napi_runtime runtime) {
//    JSContextGroupRelease((JSContextGroupRef) runtime);
    return napi_ok;
}

napi_status js_execute_script(napi_env env,
                              napi_value script,
                              const char *file,
                              napi_value *result) {

    return napi_run_script_source(env, script, file, result);

}

// [linux port] JSC drains microtasks only when the outermost VM entry returns, so jobs
// queued by C -> JS callbacks in a nested main loop would wait. WebKitGTK exports the
// JSC internals to drain them here.
namespace {
struct Drainer {
    void (*drain)(void *vm) = (void (*)(void *)) dlsym(RTLD_DEFAULT, "_ZN3JSC2VM15drainMicrotasksEv");
    void (*lock)(void *holder, void *vm) = (void (*)(void *, void *)) dlsym(RTLD_DEFAULT, "_ZN3JSC12JSLockHolderC1ERNS_2VME");
    void (*unlock)(void *holder) = (void (*)(void *)) dlsym(RTLD_DEFAULT, "_ZN3JSC12JSLockHolderD1Ev");
};
}

napi_status js_execute_pending_jobs(napi_env env) {
    static const Drainer d;
    if (d.drain && d.lock && d.unlock) {
        // JSContextGroupRef is the VM
        void *vm = (void *) JSContextGetGroup(env->context);
        alignas(16) char holder[64];
        d.lock(holder, vm);
        d.drain(vm);
        d.unlock(holder);
    }
    return napi_ok;
}

napi_status js_get_engine_ptr(napi_env env, int64_t *engine_ptr) {
    *engine_ptr = (int64_t) 0;
    return napi_ok;
}

napi_status js_adjust_external_memory(napi_env env, int64_t changeInBytes, int64_t *externalMemory) {
    return napi_ok;
}

napi_status js_cache_script(napi_env env, const char *source, const char *file) {
    return napi_ok;
}

napi_status js_run_cached_script(napi_env env, const char *file, napi_value script, void *cache,
                                 napi_value *result) {
    return napi_ok;
}


napi_status js_get_runtime_version(napi_env env, napi_value* version) {
    napi_create_string_utf8(env, "JSC", NAPI_AUTO_LENGTH, version);

    return napi_ok;
}

// [linux port] JavaScriptCore forbids a Proxy in the global's prototype chain:
// unknown globals resolve through a classed object there instead (its callbacks
// run only for names the global and its prototype lack).
namespace {
JSObjectRef lookup_has = nullptr;
JSObjectRef lookup_get = nullptr;

JSValueRef call_lookup(JSContextRef ctx, JSObjectRef fn, JSStringRef name, JSValueRef *exception) {
    JSValueRef arg = JSValueMakeString(ctx, name);
    return JSObjectCallAsFunction(ctx, fn, nullptr, 1, &arg, exception);
}

bool lookup_has_property(JSContextRef ctx, JSObjectRef, JSStringRef name) {
    JSValueRef result = call_lookup(ctx, lookup_has, name, nullptr);
    return result && JSValueToBoolean(ctx, result);
}

JSValueRef lookup_get_property(JSContextRef ctx, JSObjectRef, JSStringRef name, JSValueRef *exception) {
    return call_lookup(ctx, lookup_get, name, exception);
}
}

extern "C" napi_status js_set_global_lookup(napi_env env, napi_value has, napi_value get) {
    JSContextRef ctx = env->context;
    lookup_has = (JSObjectRef) has;
    lookup_get = (JSObjectRef) get;
    JSValueProtect(ctx, lookup_has);
    JSValueProtect(ctx, lookup_get);
    JSClassDefinition def = kJSClassDefinitionEmpty;
    def.className = "GlobalLookup";
    def.attributes = kJSClassAttributeNoAutomaticPrototype;
    def.hasProperty = lookup_has_property;
    def.getProperty = lookup_get_property;
    static JSClassRef lookup_class = JSClassCreate(&def);
    JSObjectRef global = JSContextGetGlobalObject(ctx);
    // global -> its class prototype -> lookup -> Object.prototype
    JSObjectRef proto = (JSObjectRef) JSObjectGetPrototype(ctx, global);
    JSObjectRef lookup = JSObjectMake(ctx, lookup_class, nullptr);
    JSObjectSetPrototype(ctx, lookup, JSObjectGetPrototype(ctx, proto));
    JSObjectSetPrototype(ctx, proto, lookup);
    return JSObjectGetPrototype(ctx, proto) == lookup ? napi_ok : napi_generic_failure;
}
