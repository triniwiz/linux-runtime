const { eq } = require('./harness');

// struct _GHookList { gulong seq_id; guint hook_size : 16; guint is_setup : 1; ... }
const hooks = new _GHookList();
hooks.hook_size = 300;
eq([hooks.hook_size, hooks.is_setup], [300, 0], "bitfield write/read");
g_hook_list_init(hooks, 64);
eq([hooks.hook_size, hooks.is_setup], [64, 1], "bitfields written by C");
g_hook_list_clear(hooks);
