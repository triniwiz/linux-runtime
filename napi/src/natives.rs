//! Native helpers backing the shared JS preludes (same names as the V8 runtime).

use crate::js::*;
use crate::sys::*;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

type Cb = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn bytes_of<'a>(js: Js, v: napi_value) -> &'a [u8] {
    match js.buffer_of(v) {
        Some((p, len)) if !p.is_null() => unsafe { std::slice::from_raw_parts(p, len) },
        _ => &[],
    }
}

unsafe extern "C" fn print(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, argc, ..) = cb_args::<3>(env, info);
    let stream = js.i32_of(a[0]).unwrap_or(1);
    let text = js.to_string(a[1]);
    let end = if argc < 3 || js.bool_of(a[2]) {
        "\n"
    } else {
        ""
    };
    // keep ordering with C stdio (printf)
    libc::fflush(std::ptr::null_mut());
    if stream == 2 {
        let _ = write!(std::io::stderr(), "{text}{end}");
    } else {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{text}{end}");
        let _ = out.flush();
    }
    std::ptr::null_mut()
}

fn url_parts(js: Js, u: &url::Url) -> napi_value {
    let values = [
        u.as_str().to_owned(),
        format!("{}:", u.scheme()),
        u.username().to_owned(),
        u.password().unwrap_or("").to_owned(),
        match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => String::new(),
        },
        u.host_str().unwrap_or("").to_owned(),
        u.port().map(|p| p.to_string()).unwrap_or_default(),
        u.path().to_owned(),
        u.query()
            .filter(|q| !q.is_empty())
            .map(|q| format!("?{q}"))
            .unwrap_or_default(),
        u.fragment()
            .filter(|f| !f.is_empty())
            .map(|f| format!("#{f}"))
            .unwrap_or_default(),
        u.origin().ascii_serialization(),
    ];
    let items: Vec<napi_value> = values.iter().map(|v| js.string(v)).collect();
    js.array(&items)
}

unsafe extern "C" fn url_parse(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<2>(env, info);
    let input = js.to_string(a[0]);
    let parsed = if matches!(js.type_of(a[1]), UNDEFINED | NULL) {
        url::Url::parse(&input)
    } else {
        url::Url::parse(&js.to_string(a[1])).and_then(|b| b.join(&input))
    };
    parsed
        .map(|u| url_parts(js, &u))
        .unwrap_or_else(|_| js.null())
}

unsafe extern "C" fn url_set(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<3>(env, info);
    let (href, part, value) = (js.to_string(a[0]), js.to_string(a[1]), js.to_string(a[2]));
    let Ok(mut u) = url::Url::parse(&href) else {
        return js.null();
    };
    let ok = match part.as_str() {
        "protocol" => u.set_scheme(value.trim_end_matches(':')).is_ok(),
        "username" => u.set_username(&value).is_ok(),
        "password" => u
            .set_password(Some(value.as_str()).filter(|v| !v.is_empty()))
            .is_ok(),
        "host" => {
            let (h, p) = value
                .rsplit_once(':')
                .map(|(h, p)| (h, p.parse().ok()))
                .unwrap_or((&value, None));
            u.set_host(Some(h)).is_ok() && u.set_port(p).is_ok()
        }
        "hostname" => u.set_host(Some(&value)).is_ok(),
        "port" => u
            .set_port(if value.is_empty() {
                None
            } else {
                value.parse().ok()
            })
            .is_ok(),
        "pathname" => {
            u.set_path(&value);
            true
        }
        "search" => {
            let q = value.trim_start_matches('?');
            u.set_query(Some(q).filter(|q| !q.is_empty()));
            true
        }
        "hash" => {
            let f = value.trim_start_matches('#');
            u.set_fragment(Some(f).filter(|f| !f.is_empty()));
            true
        }
        _ => false,
    };
    if ok {
        url_parts(js, &u)
    } else {
        js.null()
    }
}

unsafe extern "C" fn text_encode(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<1>(env, info);
    let s = if js.type_of(a[0]) == UNDEFINED {
        String::new()
    } else {
        js.to_string(a[0])
    };
    js.uint8array(s.as_bytes())
}

unsafe extern "C" fn text_decode(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<3>(env, info);
    let bytes = bytes_of(js, a[0]);
    let bytes = if js.bool_of(a[2]) {
        bytes
    } else {
        bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
    };
    if js.bool_of(a[1]) {
        match std::str::from_utf8(bytes) {
            Ok(s) => js.string(s),
            Err(_) => {
                js.throw_type("The encoded data was not valid for encoding utf-8");
                std::ptr::null_mut()
            }
        }
    } else {
        js.string(&String::from_utf8_lossy(bytes))
    }
}

/// textEncodeInto(string, view) -> [read UTF-16 units, written bytes]
unsafe extern "C" fn text_encode_into(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<2>(env, info);
    let text = js.to_string(a[0]);
    let Some((p, len)) = js.buffer_of(a[1]).filter(|(p, _)| !p.is_null()) else {
        return js.array(&[js.number(0.0), js.number(0.0)]);
    };
    let mut written = text.len().min(len);
    while !text.is_char_boundary(written) {
        written -= 1;
    }
    std::ptr::copy_nonoverlapping(text.as_ptr(), p as *mut u8, written);
    let read = text[..written].encode_utf16().count();
    js.array(&[js.number(read as f64), js.number(written as f64)])
}

/// Latin-1 bytes of a string, None if it has characters above U+00FF.
unsafe fn latin1_of(env: napi_env, v: napi_value) -> Option<Vec<u8>> {
    let mut len = 0;
    if napi_get_value_string_utf16(env, v, std::ptr::null_mut(), 0, &mut len) != 0 {
        return None;
    }
    let mut units = vec![0u16; len + 1];
    if napi_get_value_string_utf16(env, v, units.as_mut_ptr(), units.len(), &mut len) != 0 {
        return None;
    }
    units[..len].iter().map(|u| u8::try_from(*u).ok()).collect()
}

unsafe fn latin1_string(env: napi_env, bytes: &[u8]) -> napi_value {
    let mut out = std::ptr::null_mut();
    napi_create_string_latin1(env, bytes.as_ptr() as *const _, bytes.len(), &mut out);
    out
}

/// btoa(string) -> base64, undefined for characters above U+00FF
unsafe extern "C" fn btoa(env: napi_env, info: napi_callback_info) -> napi_value {
    let (a, ..) = cb_args::<1>(env, info);
    match latin1_of(env, a[0]) {
        Some(bytes) => latin1_string(env, &nativescript_core::base64::encode(&bytes)),
        None => Js(env).undefined(),
    }
}

/// atob(string) -> binary string, undefined for invalid input
unsafe extern "C" fn atob(env: napi_env, info: napi_callback_info) -> napi_value {
    let (a, ..) = cb_args::<1>(env, info);
    match latin1_of(env, a[0]).and_then(|b| nativescript_core::base64::decode(&b)) {
        Some(bytes) => latin1_string(env, &bytes),
        None => Js(env).undefined(),
    }
}

unsafe extern "C" fn random_fill(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<1>(env, info);
    let Some((p, len)) = js.buffer_of(a[0]) else {
        js.throw("expected a typed array");
        return std::ptr::null_mut();
    };
    if len > 65536 {
        js.throw("QuotaExceededError: getRandomValues is limited to 65536 bytes");
        return std::ptr::null_mut();
    }
    let mut filled = 0;
    while filled < len {
        let n = libc::getrandom(p.add(filled) as *mut _, len - filled, 0);
        if n <= 0 {
            js.throw("getrandom failed");
            return std::ptr::null_mut();
        }
        filled += n as usize;
    }
    a[0]
}

unsafe extern "C" fn now(env: napi_env, _: napi_callback_info) -> napi_value {
    Js(env).number(start().elapsed().as_secs_f64() * 1000.0)
}

unsafe extern "C" fn hrtime(env: napi_env, _: napi_callback_info) -> napi_value {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    Js(env).bigint_u64(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

unsafe extern "C" fn exit(env: napi_env, info: napi_callback_info) -> napi_value {
    let (a, ..) = cb_args::<1>(env, info);
    let code = Js(env).i32_of(a[0]).unwrap_or(0);
    libc::fflush(std::ptr::null_mut());
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

unsafe extern "C" fn cwd(env: napi_env, _: napi_callback_info) -> napi_value {
    Js(env).string(
        &std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy(),
    )
}

unsafe extern "C" fn chdir(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<1>(env, info);
    let dir = js.to_string(a[0]);
    if let Err(e) = std::env::set_current_dir(&dir) {
        js.throw(&format!("chdir {dir}: {e}"));
    }
    std::ptr::null_mut()
}

unsafe extern "C" fn getenv(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<1>(env, info);
    std::env::var(js.to_string(a[0]))
        .map(|v| js.string(&v))
        .unwrap_or_else(|_| js.undefined())
}

unsafe extern "C" fn setenv(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<2>(env, info);
    let key = js.to_string(a[0]);
    // single JS thread; other threads only read the environment through libc
    if js.type_of(a[1]) == UNDEFINED {
        std::env::remove_var(&key);
    } else {
        std::env::set_var(&key, js.to_string(a[1]));
    }
    std::ptr::null_mut()
}

unsafe extern "C" fn env_keys(env: napi_env, _: napi_callback_info) -> napi_value {
    let js = Js(env);
    let keys: Vec<napi_value> = std::env::vars_os()
        .map(|(k, _)| js.string(&k.to_string_lossy()))
        .collect();
    js.array(&keys)
}

/// Object handed to the JS preludes.
pub fn create(js: Js, argv: &[String], engine: &str) -> napi_value {
    let natives = js.object();
    for (name, cb) in [
        ("print", print as Cb),
        ("urlParse", url_parse),
        ("urlSet", url_set),
        ("textEncode", text_encode),
        ("textDecode", text_decode),
        ("textEncodeInto", text_encode_into),
        ("btoa", btoa),
        ("atob", atob),
        ("randomFill", random_fill),
        ("subtle", crate::subtle::subtle),
        ("now", now),
        ("hrtime", hrtime),
        ("exit", exit),
        ("cwd", cwd),
        ("chdir", chdir),
        ("getenv", getenv),
        ("setenv", setenv),
        ("envKeys", env_keys),
    ] {
        js.set(
            natives,
            name,
            js.function(name, Some(cb), std::ptr::null_mut()),
        );
    }
    for (name, cb) in crate::gobject::NATIVES {
        js.set(natives, name, js.function(name, Some(cb), std::ptr::null_mut()));
    }
    crate::app_bundle::install(js, natives);
    let items: Vec<napi_value> = argv.iter().map(|a| js.string(a)).collect();
    js.set(natives, "argv", js.array(&items));
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    js.set(natives, "arch", js.string(arch));
    js.set(natives, "engine", js.string(engine));
    js.set(natives, "pid", js.int(std::process::id() as i32));
    let origin = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
        - start().elapsed().as_secs_f64() * 1000.0;
    js.set(natives, "timeOrigin", js.number(origin));
    natives
}
