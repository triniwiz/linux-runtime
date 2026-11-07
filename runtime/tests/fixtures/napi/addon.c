#include <node_api.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(call)                                       \
  if ((call) != napi_ok) {                                \
    napi_throw_error(env, "E_NAPI", #call " failed");     \
    return NULL;                                          \
  }

static int finalized = 0;
static char shared[16] = "zero-copy";

static napi_value Add(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2], r;
  double a, b;
  CHECK(napi_get_cb_info(env, info, &argc, argv, NULL, NULL));
  CHECK(napi_get_value_double(env, argv[0], &a));
  CHECK(napi_get_value_double(env, argv[1], &b));
  CHECK(napi_create_double(env, a + b, &r));
  return r;
}

static napi_value Greet(napi_env env, napi_callback_info info) {
  size_t argc = 1, len;
  napi_value arg, r;
  char name[64], out[96];
  CHECK(napi_get_cb_info(env, info, &argc, &arg, NULL, NULL));
  CHECK(napi_get_value_string_utf8(env, arg, name, sizeof(name), &len));
  strcpy(out, "hello ");
  strcat(out, name);
  CHECK(napi_create_string_utf8(env, out, NAPI_AUTO_LENGTH, &r));
  return r;
}

static napi_value CallWith21(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value fn, arg, undefined, r;
  CHECK(napi_get_cb_info(env, info, &argc, &fn, NULL, NULL));
  CHECK(napi_create_int32(env, 21, &arg));
  CHECK(napi_get_undefined(env, &undefined));
  if (napi_call_function(env, undefined, fn, 1, &arg, &r) != napi_ok) return NULL;  // propagate JS exception
  return r;
}

static napi_value Throw(napi_env env, napi_callback_info info) {
  napi_throw_error(env, "E_BOOM", "boom from C");
  return NULL;
}

typedef struct { int value; } Counter;

static void CounterFinalize(napi_env env, void* data, void* hint) {
  free(data);
  finalized++;
}

static napi_value CounterNew(napi_env env, napi_callback_info info) {
  napi_value self;
  CHECK(napi_get_cb_info(env, info, NULL, NULL, &self, NULL));
  Counter* c = calloc(1, sizeof(Counter));
  CHECK(napi_wrap(env, self, c, CounterFinalize, NULL, NULL));
  return self;
}

static napi_value CounterInc(napi_env env, napi_callback_info info) {
  napi_value self, r;
  Counter* c;
  CHECK(napi_get_cb_info(env, info, NULL, NULL, &self, NULL));
  CHECK(napi_unwrap(env, self, (void**)&c));
  CHECK(napi_create_int32(env, ++c->value, &r));
  return r;
}

static napi_value CounterValue(napi_env env, napi_callback_info info) {
  napi_value self, r;
  Counter* c;
  CHECK(napi_get_cb_info(env, info, NULL, NULL, &self, NULL));
  CHECK(napi_unwrap(env, self, (void**)&c));
  CHECK(napi_create_int32(env, c->value, &r));
  return r;
}

static napi_value Finalized(napi_env env, napi_callback_info info) {
  napi_value r;
  CHECK(napi_create_int32(env, finalized, &r));
  return r;
}

static napi_value ExternalBuffer(napi_env env, napi_callback_info info) {
  napi_value r;
  CHECK(napi_create_external_buffer(env, strlen(shared), shared, NULL, NULL, &r));
  return r;
}

static napi_value MutateShared(napi_env env, napi_callback_info info) {
  shared[0] = 'Z';
  return NULL;
}

static napi_value SumBytes(napi_env env, napi_callback_info info) {
  size_t argc = 1, len;
  napi_value arg, r;
  uint8_t* data;
  uint32_t sum = 0;
  CHECK(napi_get_cb_info(env, info, &argc, &arg, NULL, NULL));
  CHECK(napi_get_buffer_info(env, arg, (void**)&data, &len));
  for (size_t i = 0; i < len; i++) sum += data[i];
  data[0] = 99;  // writes are visible to JS (same memory)
  CHECK(napi_create_uint32(env, sum, &r));
  return r;
}

typedef struct {
  napi_async_work work;
  napi_deferred deferred;
  int input, output;
} Work;

static void Execute(napi_env env, void* data) {
  Work* w = data;
  w->output = w->input * 2;
}

static void Complete(napi_env env, napi_status status, void* data) {
  Work* w = data;
  napi_value v;
  napi_create_int32(env, w->output, &v);
  napi_resolve_deferred(env, w->deferred, v);
  napi_delete_async_work(env, w->work);
  free(w);
}

static napi_value DoubleAsync(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value arg, promise, name;
  Work* w = calloc(1, sizeof(Work));
  CHECK(napi_get_cb_info(env, info, &argc, &arg, NULL, NULL));
  CHECK(napi_get_value_int32(env, arg, &w->input));
  CHECK(napi_create_promise(env, &w->deferred, &promise));
  CHECK(napi_create_string_utf8(env, "double", NAPI_AUTO_LENGTH, &name));
  CHECK(napi_create_async_work(env, NULL, name, Execute, Complete, w, &w->work));
  CHECK(napi_queue_async_work(env, w->work));
  return promise;
}

static void CallJs(napi_env env, napi_value js_cb, void* context, void* data) {
  napi_value v, undefined;
  napi_create_int32(env, (int)(intptr_t)data, &v);
  napi_get_undefined(env, &undefined);
  napi_call_function(env, undefined, js_cb, 1, &v, NULL);
}

static void* ThreadMain(void* arg) {
  napi_threadsafe_function tsfn = arg;
  for (intptr_t i = 1; i <= 3; i++) napi_call_threadsafe_function(tsfn, (void*)i, napi_tsfn_blocking);
  napi_release_threadsafe_function(tsfn, napi_tsfn_release);
  return NULL;
}

static napi_value FromThread(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value fn, name;
  napi_threadsafe_function tsfn;
  pthread_t t;
  CHECK(napi_get_cb_info(env, info, &argc, &fn, NULL, NULL));
  CHECK(napi_create_string_utf8(env, "tsfn", NAPI_AUTO_LENGTH, &name));
  CHECK(napi_create_threadsafe_function(env, fn, NULL, name, 0, 1, NULL, NULL, NULL, CallJs, &tsfn));
  pthread_create(&t, NULL, ThreadMain, tsfn);
  pthread_detach(t);
  return NULL;
}

static napi_ref stored = NULL;

static napi_value Store(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value arg;
  CHECK(napi_get_cb_info(env, info, &argc, &arg, NULL, NULL));
  if (stored) napi_delete_reference(env, stored);
  CHECK(napi_create_reference(env, arg, 1, &stored));
  return NULL;
}

static napi_value Load(napi_env env, napi_callback_info info) {
  napi_value r;
  CHECK(napi_get_reference_value(env, stored, &r));
  return r;
}

static napi_value Describe(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value arg, keys, r;
  napi_valuetype t;
  uint32_t n;
  CHECK(napi_get_cb_info(env, info, &argc, &arg, NULL, NULL));
  CHECK(napi_typeof(env, arg, &t));
  if (t != napi_object) {
    CHECK(napi_create_int32(env, -(int)t, &r));
    return r;
  }
  CHECK(napi_get_property_names(env, arg, &keys));
  CHECK(napi_get_array_length(env, keys, &n));
  CHECK(napi_create_uint32(env, n, &r));
  return r;
}

static napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor props[] = {
      {"add", NULL, Add, NULL, NULL, NULL, napi_default, NULL},
      {"greet", NULL, Greet, NULL, NULL, NULL, napi_default, NULL},
      {"callWith21", NULL, CallWith21, NULL, NULL, NULL, napi_default, NULL},
      {"throws", NULL, Throw, NULL, NULL, NULL, napi_default, NULL},
      {"finalized", NULL, Finalized, NULL, NULL, NULL, napi_default, NULL},
      {"externalBuffer", NULL, ExternalBuffer, NULL, NULL, NULL, napi_default, NULL},
      {"mutateShared", NULL, MutateShared, NULL, NULL, NULL, napi_default, NULL},
      {"sumBytes", NULL, SumBytes, NULL, NULL, NULL, napi_default, NULL},
      {"doubleAsync", NULL, DoubleAsync, NULL, NULL, NULL, napi_default, NULL},
      {"fromThread", NULL, FromThread, NULL, NULL, NULL, napi_default, NULL},
      {"store", NULL, Store, NULL, NULL, NULL, napi_default, NULL},
      {"load", NULL, Load, NULL, NULL, NULL, napi_default, NULL},
      {"describe", NULL, Describe, NULL, NULL, NULL, napi_default, NULL},
  };
  napi_property_descriptor counter_props[] = {
      {"inc", NULL, CounterInc, NULL, NULL, NULL, napi_default_method, NULL},
      {"value", NULL, NULL, CounterValue, NULL, NULL, napi_default, NULL},
  };
  napi_value counter;
  CHECK(napi_define_properties(env, exports, sizeof(props) / sizeof(props[0]), props));
  CHECK(napi_define_class(env, "Counter", NAPI_AUTO_LENGTH, CounterNew, NULL, 2, counter_props, &counter));
  CHECK(napi_set_named_property(env, exports, "Counter", counter));
  return exports;
}

NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)
