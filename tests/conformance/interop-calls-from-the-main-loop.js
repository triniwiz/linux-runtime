const { eq } = require('./harness');

// C calling JS from the runtime's own loop, outside any JS call: async callbacks, vfuncs (also
// from a worker thread), signal handlers and property bindings. Each case hands JS a GObject
// of a native class it hasn't seen yet, whose prototype is made right there.

const steps = [];
const step = (name, run) => steps.push({ name, run });
let deadline;
const next = () => {
  clearTimeout(deadline);
  const s = steps.shift();
  if (!s) return;
  deadline = setTimeout(() => { throw new Error(`${s.name}: not called from the loop`); }, 5000);
  s.run(next);
};

const connect = (family, type, protocol, address) => {
  const client = g_socket_new(family, type, protocol, null);
  eq(g_socket_connect(client, address, null, null), 1, 'client connected');
  return client;
};

step('async callback', (done) => {
  const file = g_file_new_for_path('/');
  g_file_query_info_async(file, 'standard::type', 0, G_PRIORITY_DEFAULT, null, (source, result) => {
    eq([source === file, result instanceof GTask, result instanceof GObject], [true, true, true], 'callback arguments');
    const info = g_file_query_info_finish(source, result, null);
    eq(g_file_info_get_file_type(info), G_FILE_TYPE_DIRECTORY, 'async result');
    done();
  }, null);
});

step('vfunc', (done) => {
  class Service extends GSocketService {
    incoming(connection, source) {
      eq([this === service, connection instanceof GTcpConnection, source === marker], [true, true, true], 'vfunc arguments');
      g_io_stream_close(connection, null, null);
      g_socket_service_stop(this);
      done();
      return 1;
    }
  }
  const service = new Service();
  const marker = new GObject();
  const port = g_socket_listener_add_any_inet_port(service, marker, null);
  const client = connect(G_SOCKET_FAMILY_IPV4, G_SOCKET_TYPE_STREAM, G_SOCKET_PROTOCOL_TCP, g_inet_socket_address_new_from_string('127.0.0.1', port));
  setTimeout(() => g_socket_close(client, null), 1000);
});

step('vfunc called on a worker thread', (done) => {
  const js = g_thread_self().toString();
  class Service extends GThreadedSocketService {
    run(connection, source) {
      eq([g_thread_self().toString() === js, this === service, connection instanceof GUnixConnection], [true, true, true], 'run on the JS thread');
      g_io_stream_close(connection, null, null);
      g_socket_service_stop(this);
      done();
      return 1;
    }
  }
  const service = new Service({ maxThreads: 1 });
  const name = `ns-conformance-${Math.random().toString(36).slice(2)}`;
  const address = g_unix_socket_address_new_with_type(name, -1, G_UNIX_SOCKET_ADDRESS_ABSTRACT);
  eq(g_socket_listener_add_address(service, address, G_SOCKET_TYPE_STREAM, G_SOCKET_PROTOCOL_DEFAULT, null, null, null), 1, 'listening');
  const client = connect(G_SOCKET_FAMILY_UNIX, G_SOCKET_TYPE_STREAM, G_SOCKET_PROTOCOL_DEFAULT, address);
  setTimeout(() => g_socket_close(client, null), 1000);
});

step('signal handler, object of a JS class made by C', (done) => {
  let made = 0;
  class Connection extends GSocketConnection {
    constructor() {
      super();
      made++;
    }
  }
  // a class registers its GType on its first instance, or here
  __nsGObject.register(Connection);
  // a socket type without a connection class of GIO's: replacing one frees it in GLib
  g_socket_connection_factory_register_type(Connection.$gtype, G_SOCKET_FAMILY_UNIX, G_SOCKET_TYPE_SEQPACKET, G_SOCKET_PROTOCOL_DEFAULT);
  const service = g_socket_service_new();
  g_signal_connect(service, 'incoming', (self, connection, source) => {
    eq([self === service, connection instanceof Connection, made], [true, true, 1], 'handler arguments');
    eq([source instanceof GSimpleActionGroup, interop.handleof(source).equals(group)], [true, true], 'source object');
    g_io_stream_close(connection, null, null);
    g_socket_service_stop(self);
    done();
    return 1;
  });
  // held as a Pointer until the handler: g_object_new returns gpointer
  const group = g_object_new(g_simple_action_group_get_type(), null);
  const name = `ns-conformance-${Math.random().toString(36).slice(2)}`;
  const address = g_unix_socket_address_new_with_type(name, -1, G_UNIX_SOCKET_ADDRESS_ABSTRACT);
  eq(g_socket_listener_add_address(service, address, G_SOCKET_TYPE_SEQPACKET, G_SOCKET_PROTOCOL_DEFAULT, group, null, null), 1, 'listening');
  const client = connect(G_SOCKET_FAMILY_UNIX, G_SOCKET_TYPE_SEQPACKET, G_SOCKET_PROTOCOL_DEFAULT, address);
  setTimeout(() => g_socket_close(client, null), 1000);
});

step('property binding', (done) => {
  class Target extends GObject {
    static properties = { resolver: { type: g_proxy_resolver_get_type() } };
  }
  const target = new Target();
  const source = g_socket_client_new();
  g_object_bind_property(source, 'proxy-resolver', target, 'resolver', G_BINDING_DEFAULT);
  // a resolver JS only holds as a Pointer: g_object_new returns gpointer
  const resolver = g_object_new(g_simple_proxy_resolver_get_type(), null);
  const value = new GValue();
  g_value_init(value, g_proxy_resolver_get_type());
  g_value_set_object(value, resolver);
  g_object_freeze_notify(source);
  g_object_set_property(source, 'proxy-resolver', value);
  eq(target.resolver, undefined, 'not bound while notifications are frozen');
  // thawed by C from the loop, without g_idle_add_once (GLib 2.74): the source returns
  // FALSE (not floating) and its destroy notify thaws
  g_idle_add_full(G_PRIORITY_DEFAULT_IDLE, g_object_is_floating, source, g_object_thaw_notify);
  setTimeout(() => {
    eq([target.resolver instanceof GSimpleProxyResolver, interop.handleof(target.resolver).equals(resolver)], [true, true], 'bound from the loop');
    done();
  }, 50);
});

next();
