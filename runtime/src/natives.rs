//! Native helpers backing the JS preludes (console, URL, TextEncoder, process...).

use crate::throw_error;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

fn str<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap()
}

pub(crate) fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn print<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let stream = args.get(0).int32_value(scope).unwrap_or(1);
    let text = args.get(1).to_rust_string_lossy(scope);
    let newline = args.length() < 3 || args.get(2).boolean_value(scope);
    // keep ordering with C stdio (printf)
    unsafe { libc::fflush(std::ptr::null_mut()) };
    let end = if newline { "\n" } else { "" };
    if stream == 2 {
        let _ = write!(std::io::stderr(), "{text}{end}");
    } else {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{text}{end}");
        let _ = out.flush();
    }
}

fn url_parts<'s>(scope: &mut v8::PinScope<'s, '_>, u: &url::Url) -> v8::Local<'s, v8::Value> {
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
    ];
    let mut items: Vec<v8::Local<v8::Value>> =
        values.iter().map(|v| str(scope, v).into()).collect();
    items.push(str(scope, &u.origin().ascii_serialization()).into());
    v8::Array::new_with_elements(scope, &items).into()
}

/// urlParse(input, base?) -> [href, protocol, ..., origin] | null
fn url_parse<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let input = args.get(0).to_rust_string_lossy(scope);
    let parsed = if args.get(1).is_null_or_undefined() {
        url::Url::parse(&input)
    } else {
        let base = args.get(1).to_rust_string_lossy(scope);
        url::Url::parse(&base).and_then(|b| b.join(&input))
    };
    match parsed {
        Ok(u) => rv.set(url_parts(scope, &u)),
        Err(_) => rv.set_null(),
    }
}

/// urlSet(href, part, value) -> parts | null
fn url_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let href = args.get(0).to_rust_string_lossy(scope);
    let part = args.get(1).to_rust_string_lossy(scope);
    let value = args.get(2).to_rust_string_lossy(scope);
    let Ok(mut u) = url::Url::parse(&href) else {
        return rv.set_null();
    };
    let ok = match part.as_str() {
        "protocol" => u.set_scheme(value.trim_end_matches(':')).is_ok(),
        "username" => u.set_username(&value).is_ok(),
        "password" => u
            .set_password(Some(&value).filter(|v| !v.is_empty()).map(|v| v.as_str()))
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
            u.set_query(if q.is_empty() { None } else { Some(q) });
            true
        }
        "hash" => {
            let f = value.trim_start_matches('#');
            u.set_fragment(if f.is_empty() { None } else { Some(f) });
            true
        }
        _ => false,
    };
    if ok {
        rv.set(url_parts(scope, &u));
    } else {
        rv.set_null();
    }
}

/// UTF-8 in one pass, moved into the buffer (lone surrogates become U+FFFD)
fn text_encode<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let s = if args.get(0).is_undefined() {
        String::new()
    } else {
        args.get(0).to_rust_string_lossy(scope)
    };
    let len = s.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(s.into_bytes()).make_shared();
    let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
    rv.set(v8::Uint8Array::new(scope, buffer, 0, len).unwrap().into());
}

/// textEncodeInto(string, view) -> [read UTF-16 units, written bytes]; never splits a character
fn text_encode_into<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let Some(text) = args.get(0).to_string(scope) else {
        return;
    };
    let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(args.get(1)) else {
        return throw_error(scope, "expected a Uint8Array");
    };
    let out = unsafe { std::slice::from_raw_parts_mut(view.data() as *mut u8, view.byte_length()) };
    let mut read = 0;
    let written = text.write_utf8_v2(
        scope,
        out,
        v8::WriteFlags::kReplaceInvalidUtf8,
        Some(&mut read),
    );
    let pair = [
        v8::Number::new(scope, read as f64).into(),
        v8::Number::new(scope, written as f64).into(),
    ];
    rv.set(v8::Array::new_with_elements(scope, &pair).into());
}

fn bytes_of<'a>(v: v8::Local<'a, v8::Value>) -> &'a [u8] {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        if view.byte_length() == 0 {
            return &[];
        }
        return unsafe { std::slice::from_raw_parts(view.data() as *const u8, view.byte_length()) };
    }
    if let Ok(buf) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        if let Some(p) = buf.data() {
            return unsafe {
                std::slice::from_raw_parts(p.as_ptr() as *const u8, buf.byte_length())
            };
        }
    }
    &[]
}

/// textDecode(bufferOrView, fatal, ignoreBOM)
fn text_decode<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let bytes = bytes_of(args.get(0));
    let bytes = if args.get(2).boolean_value(scope) {
        bytes
    } else {
        bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
    };
    if args.get(1).boolean_value(scope) && std::str::from_utf8(bytes).is_err() {
        let msg = str(scope, "The encoded data was not valid for encoding utf-8");
        let e = v8::Exception::type_error(scope, msg);
        scope.throw_exception(e);
        return;
    }
    // V8 decodes (and replaces invalid sequences) in one pass
    match v8::String::new_from_utf8(scope, bytes, v8::NewStringType::Normal) {
        Some(s) => rv.set(s.into()),
        None => throw_error(scope, "string too long"),
    }
}

fn one_byte_string<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bytes: &[u8],
) -> Option<v8::Local<'s, v8::String>> {
    v8::String::new_from_one_byte(scope, bytes, v8::NewStringType::Normal)
}

/// Runs `f` on the string's Latin-1 bytes (borrowed when V8 stores it as one-byte),
/// `None` if it has characters above U+00FF.
fn with_latin1<'s, R>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    f: impl FnOnce(&[u8]) -> R,
) -> Option<R> {
    let text = value.to_string(scope)?;
    let view = v8::ValueView::new(scope, text);
    match view.data() {
        v8::ValueViewData::OneByte(bytes) => Some(f(bytes)),
        v8::ValueViewData::TwoByte(units) => {
            let bytes: Option<Vec<u8>> = units.iter().map(|u| u8::try_from(*u).ok()).collect();
            bytes.map(|b| f(&b))
        }
    }
}

/// btoa(string) -> base64, undefined for characters above U+00FF
fn btoa<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let Some(encoded) = with_latin1(scope, args.get(0), nativescript_core::base64::encode) else {
        return;
    };
    if let Some(s) = one_byte_string(scope, &encoded) {
        rv.set(s.into());
    }
}

/// atob(string) -> binary string, undefined for invalid input
fn atob<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let Some(Some(decoded)) = with_latin1(scope, args.get(0), nativescript_core::base64::decode)
    else {
        return;
    };
    if let Some(s) = one_byte_string(scope, &decoded) {
        rv.set(s.into());
    }
}

fn random_fill<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(args.get(0)) else {
        return throw_error(scope, "expected a typed array");
    };
    if view.byte_length() > 65536 {
        return throw_error(
            scope,
            "QuotaExceededError: getRandomValues is limited to 65536 bytes",
        );
    }
    let mut filled = 0;
    while filled < view.byte_length() {
        let n = unsafe {
            libc::getrandom(
                (view.data() as *mut u8).add(filled) as *mut _,
                view.byte_length() - filled,
                0,
            )
        };
        if n <= 0 {
            return throw_error(scope, "getrandom failed");
        }
        filled += n as usize;
    }
    rv.set(args.get(0));
}

/// Milliseconds since process start (monotonic).
fn now<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    rv.set(v8::Number::new(scope, start().elapsed().as_secs_f64() * 1000.0).into());
}

fn hrtime<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    let ns = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
    rv.set(v8::BigInt::new_from_u64(scope, ns).into());
}

fn exit<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let code = args.get(0).int32_value(scope).unwrap_or(0);
    unsafe { libc::fflush(std::ptr::null_mut()) };
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

/// Full GC (`__collect`).
fn inspector_active<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    rv.set_bool(crate::inspector::is_active(&crate::state::state(scope)));
}

/// `(domain, dispatch)`: CDP messages for `domain` go to `dispatch(message)`.
fn inspector_register<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let domain = args.get(0).to_rust_string_lossy(scope);
    let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(1)) else {
        return;
    };
    let f = v8::Global::new(scope, f);
    crate::inspector::register_js_domain(&crate::state::state(scope), domain, f);
}

fn inspector_send<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let message = args.get(0).to_rust_string_lossy(scope);
    crate::inspector::send_from_js(&crate::state::state(scope), message);
}

fn gc<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, _rv: Rv<'s>) {
    scope.low_memory_notification();
}

fn cwd<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    let dir = std::env::current_dir().unwrap_or_default();
    rv.set(str(scope, &dir.to_string_lossy()).into());
}

fn chdir<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let dir = args.get(0).to_rust_string_lossy(scope);
    if let Err(e) = std::env::set_current_dir(&dir) {
        throw_error(scope, &format!("chdir {dir}: {e}"));
    }
}

fn getenv<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let key = args.get(0).to_rust_string_lossy(scope);
    match std::env::var(&key) {
        Ok(v) => rv.set(str(scope, &v).into()),
        Err(_) => rv.set_undefined(),
    }
}

fn setenv<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let key = args.get(0).to_rust_string_lossy(scope);
    // single JS thread; other threads only read the environment through libc
    unsafe {
        if args.get(1).is_undefined() {
            std::env::remove_var(&key);
        } else {
            let value = args.get(1).to_rust_string_lossy(scope);
            std::env::set_var(&key, value);
        }
    }
}

fn env_keys<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    let keys: Vec<v8::Local<v8::Value>> = std::env::vars_os()
        .map(|(k, _)| str(scope, &k.to_string_lossy()).into())
        .collect();
    rv.set(v8::Array::new_with_elements(scope, &keys).into());
}

/// Object handed to the JS preludes.
pub fn create<'s>(scope: &mut v8::PinScope<'s, '_>, argv: &[String]) -> v8::Local<'s, v8::Object> {
    let natives = v8::Object::new(scope);
    macro_rules! set {
        ($name:literal, $f:expr) => {{
            let f = v8::Function::new(scope, $f).unwrap();
            let k = str(scope, $name);
            natives.set(scope, k.into(), f.into());
        }};
    }
    set!("print", print);
    set!("urlParse", url_parse);
    set!("urlSet", url_set);
    set!("textEncode", text_encode);
    set!("textDecode", text_decode);
    set!("textEncodeInto", text_encode_into);
    set!("btoa", btoa);
    set!("atob", atob);
    set!("randomFill", random_fill);
    set!("now", now);
    set!("hrtime", hrtime);
    set!("exit", exit);
    set!("gc", gc);
    set!("inspectorActive", inspector_active);
    set!("inspectorRegister", inspector_register);
    set!("inspectorSend", inspector_send);
    set!("cwd", cwd);
    set!("chdir", chdir);
    set!("getenv", getenv);
    set!("setenv", setenv);
    set!("envKeys", env_keys);

    let items: Vec<v8::Local<v8::Value>> = argv.iter().map(|a| str(scope, a).into()).collect();
    let argv = v8::Array::new_with_elements(scope, &items);
    let k = str(scope, "argv");
    natives.set(scope, k.into(), argv.into());
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    for (k, v) in [
        ("arch", arch.to_owned()),
        ("v8", v8::V8::get_version().to_owned()),
    ] {
        let k = str(scope, k);
        let v = str(scope, &v);
        natives.set(scope, k.into(), v.into());
    }
    let k = str(scope, "pid");
    let v = v8::Integer::new(scope, std::process::id() as i32);
    natives.set(scope, k.into(), v.into());
    let k = str(scope, "timeOrigin");
    let origin = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
        - start().elapsed().as_secs_f64() * 1000.0;
    let v = v8::Number::new(scope, origin);
    natives.set(scope, k.into(), v.into());
    natives
}
