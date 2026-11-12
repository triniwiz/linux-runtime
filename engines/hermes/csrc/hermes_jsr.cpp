// napi-android's js_* host API over Hermes and its Node-API (hermes-windows'
// hermes_node_api), replacing hermes_api.cpp, which is tied to Windows (WER,
// inspector stubs).

#include "hermes/BCGen/HBC/BCProviderFromSrc.h"
#include "hermes/VM/Callable.h"
#include "hermes/VM/Runtime.h"
#include "hermes/hermes.h"
#include "hermes_node_api.h"

#include <cstdio>
#include <deque>
#include <memory>
#include <string>

namespace hermes::node_api {
// added to hermes_node_api.cpp by build-hermes.sh
napi_status runPendingFinalizers(napi_env env) noexcept;
} // namespace hermes::node_api

namespace {

using ::hermes::vm::CallResult;
using ::hermes::vm::ExecutionStatus;
using ::hermes::vm::HermesValue;

// Node-API finalizers posted from the GC, run by js_execute_pending_jobs.
class TaskQueue final : public ::hermes::node_api::TaskRunner {
 public:
  void post(std::unique_ptr<::hermes::node_api::Task> task) noexcept override {
    tasks_.push_back(std::move(task));
  }

  void run() {
    while (!tasks_.empty()) {
      auto task = std::move(tasks_.front());
      tasks_.pop_front();
      task->invoke();
    }
  }

 private:
  std::deque<std::unique_ptr<::hermes::node_api::Task>> tasks_;
};

class StringBuffer final : public ::hermes::Buffer {
 public:
  explicit StringBuffer(std::string s) : s_(std::move(s)) {
    data_ = reinterpret_cast<const uint8_t *>(s_.c_str());
    size_ = s_.size();
  }

 private:
  std::string s_;
};

class BorrowedBuffer final : public ::hermes::Buffer {
 public:
  BorrowedBuffer(const uint8_t *data, size_t size) : Buffer(data, size) {}
};

} // namespace

struct napi_runtime__ {
  std::unique_ptr<facebook::hermes::HermesRuntime> jsi;
  ::hermes::vm::Runtime *vm{};
  std::shared_ptr<TaskQueue> tasks = std::make_shared<TaskQueue>();
  ::hermes::hbc::CompileFlags flags{};
  napi_env env{};
};
typedef napi_runtime__ *napi_runtime;

// Hermes builds with -fvisibility=hidden
#define JS_API __attribute__((visibility("default")))

namespace {

thread_local napi_runtime__ *current = nullptr;

::hermes::vm::RuntimeConfig runtimeConfig() {
  return ::hermes::vm::RuntimeConfig::Builder()
      // promise jobs run from js_execute_pending_jobs, not setImmediate
      .withMicrotaskQueue(true)
      .withES6BlockScoping(true)
      .withEnableAsyncGenerators(true)
      .withES6Proxy(true)
      .build();
}

napi_status runBytecode(
    napi_env env,
    std::unique_ptr<::hermes::hbc::BCProvider> bc,
    bool persistent,
    const char *file,
    napi_value *result) {
  ::hermes::vm::RuntimeModuleFlags flags{};
  flags.persistent = persistent;
  ::hermes::vm::Runtime &vm = *current->vm;
  std::string url = file ? file : "";
  return ::hermes::node_api::runInNodeApiContext(
      env,
      [&]() -> CallResult<HermesValue> {
        return vm.runBytecode(
            std::move(bc),
            flags,
            url,
            ::hermes::vm::Runtime::makeNullHandle<::hermes::vm::Environment>());
      },
      result);
}

// Hermes' Promise reports through its rejection tracker (as in React Native,
// after a 100ms / 2s timeout); forwarded to the host's
// onUnhandledPromiseRejectionTracker(key, reason, handled).
const char *kRejections = R"(
(function () {
  var hi = typeof HermesInternal === 'object' && HermesInternal;
  if (!hi || !hi.enablePromiseRejectionTracker) return;
  var track = function (id, reason, handled) {
    var t = globalThis.onUnhandledPromiseRejectionTracker;
    if (typeof t === 'function') t(id, reason, handled);
  };
  hi.enablePromiseRejectionTracker({
    allRejections: true,
    onUnhandled: function (id, e) { track(id, e, false); },
    onHandled: function (id) { track(id, undefined, true); },
  });
})();
)";

extern "C" napi_status js_execute_script(
    napi_env env, napi_value script, const char *file, napi_value *result);

napi_status throwSyntaxError(napi_env env, const std::string &message) {
  napi_value text, error;
  napi_create_string_utf8(env, message.c_str(), message.size(), &text);
  napi_value global, ctor;
  napi_get_global(env, &global);
  napi_get_named_property(env, global, "SyntaxError", &ctor);
  if (napi_new_instance(env, ctor, 1, &text, &error) != napi_ok)
    napi_create_error(env, nullptr, text, &error);
  napi_throw(env, error);
  return napi_pending_exception;
}

} // namespace

extern "C" {

JS_API napi_status js_create_runtime(napi_runtime *runtime) {
  if (!runtime)
    return napi_invalid_arg;
  auto *rt = new napi_runtime__();
  rt->jsi = facebook::hermes::makeHermesRuntime(runtimeConfig());
  auto *hermes =
      facebook::jsi::castInterface<facebook::hermes::IHermes>(rt->jsi.get());
  rt->vm = static_cast<::hermes::vm::Runtime *>(hermes->getVMRuntimeUnsafe());
  rt->flags.lazy = true;
  rt->flags.enableGenerator = true;
  rt->flags.enableES6BlockScoping = true;
  rt->flags.enableAsyncGenerators = true;
  *runtime = rt;
  return napi_ok;
}

JS_API napi_status js_create_napi_env(napi_env *env, napi_runtime runtime) {
  if (!env || !runtime)
    return napi_invalid_arg;
  auto res = ::hermes::node_api::getOrCreateNodeApiEnvironment(
      *runtime->vm,
      runtime->flags,
      runtime->tasks,
      [](napi_env, napi_value) {
        fprintf(stderr, "Hermes: unhandled fatal Node-API exception\n");
      },
      NAPI_VERSION_EXPERIMENTAL);
  if (res.getStatus() == ExecutionStatus::EXCEPTION)
    return napi_generic_failure;
  runtime->env = *res;
  current = runtime;
  *env = runtime->env;
  napi_handle_scope scope;
  napi_open_handle_scope(*env, &scope);
  napi_value code, result;
  napi_create_string_utf8(*env, kRejections, NAPI_AUTO_LENGTH, &code);
  js_execute_script(*env, code, "nativescript:hermes", &result);
  napi_close_handle_scope(*env, scope);
  return napi_ok;
}

// the env lives as long as its runtime
JS_API napi_status js_free_napi_env(napi_env) {
  return napi_ok;
}

JS_API napi_status js_free_runtime(napi_runtime runtime) {
  if (current == runtime)
    current = nullptr;
  delete runtime;
  return napi_ok;
}

JS_API napi_status js_execute_script(
    napi_env env,
    napi_value script,
    const char *file,
    napi_value *result) {
  if (!current || !script || !result)
    return napi_invalid_arg;
  size_t len = 0;
  if (napi_get_value_string_utf8(env, script, nullptr, 0, &len) != napi_ok)
    return napi_string_expected;
  std::string code(len, '\0');
  napi_get_value_string_utf8(env, script, code.data(), len + 1, nullptr);
  auto bc = ::hermes::hbc::createBCProviderFromSrc(
      std::make_unique<StringBuffer>(std::move(code)),
      file ? file : "",
      {},
      current->flags);
  if (!bc.first)
    return throwSyntaxError(env, bc.second);
  return runBytecode(env, std::move(bc.first), false, file, result);
}

// Runs hermesc output; `data` must outlive the runtime.
JS_API napi_status js_execute_bytecode(
    napi_env env,
    const uint8_t *data,
    size_t size,
    const char *file,
    napi_value *result) {
  if (!current || !data || !result)
    return napi_invalid_arg;
  auto bc = ::hermes::hbc::BCProviderFromBuffer::createBCProviderFromBuffer(
      std::make_unique<BorrowedBuffer>(data, size));
  if (!bc.first)
    return throwSyntaxError(env, bc.second);
  return runBytecode(env, std::move(bc.first), true, file, result);
}

JS_API int js_is_bytecode(const uint8_t *data, size_t size) {
  auto *api = facebook::jsi::castInterface<facebook::hermes::IHermesRootAPI>(
      facebook::hermes::makeHermesRootAPI());
  return api->isHermesBytecode(data, size) ? 1 : 0;
}

// Promise jobs, then finalizers. A throwing job leaves its exception pending.
JS_API napi_status js_execute_pending_jobs(napi_env env) {
  if (!current)
    return napi_invalid_arg;
  ::hermes::vm::Runtime &vm = *current->vm;
  if (vm.hasMicrotaskQueue()) {
    napi_status status =
        ::hermes::node_api::checkJSErrorStatus(env, vm.drainJobs());
    if (status != napi_ok)
      return status;
  }
  vm.clearKeptObjects();
  current->tasks->run();
  return ::hermes::node_api::runPendingFinalizers(env);
}

JS_API napi_status js_get_runtime_version(napi_env env, napi_value *version) {
  return napi_create_string_utf8(env, "Hermes", NAPI_AUTO_LENGTH, version);
}

} // extern "C"
