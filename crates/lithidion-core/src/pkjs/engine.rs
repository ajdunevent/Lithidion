//! The little of JavaScriptCore's GLib API the runner needs that the bindings leave out: a JS
//! function backed by a Rust closure.

use glib::translate::{from_glib_none, IntoGlibPtr, ToGlibPtr};
use javascriptcore6 as jsc;
use std::ffi::c_void;

type Callback = Box<dyn Fn(&[jsc::Value]) -> Option<jsc::Value>>;

unsafe extern "C" fn call(args: *mut glib::ffi::GPtrArray, data: glib::ffi::gpointer) -> *mut jsc::ffi::JSCValue {
    let f = &*(data as *const Callback);
    let args: Vec<jsc::Value> = if args.is_null() {
        Vec::new()
    } else {
        let a = &*args;
        (0..a.len as usize).map(|i| from_glib_none(*a.pdata.add(i) as *mut jsc::ffi::JSCValue)).collect()
    };
    match f(&args) {
        Some(v) => v.into_glib_ptr(),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn free(data: glib::ffi::gpointer) {
    drop(Box::from_raw(data as *mut Callback));
}

/// A function taking any number of arguments. Returning `None` gives `undefined`.
pub fn function(ctx: &jsc::Context, name: &str, f: impl Fn(&[jsc::Value]) -> Option<jsc::Value> + 'static) -> jsc::Value {
    let data: Box<Callback> = Box::new(Box::new(f));
    unsafe {
        let trampoline: unsafe extern "C" fn(*mut glib::ffi::GPtrArray, glib::ffi::gpointer) -> *mut jsc::ffi::JSCValue = call;
        let value = jsc::ffi::jsc_value_new_function_variadic(
            ctx.to_glib_none().0,
            name.to_glib_none().0,
            Some(std::mem::transmute::<*const c_void, unsafe extern "C" fn()>(trampoline as *const c_void)),
            Box::into_raw(data) as glib::ffi::gpointer,
            Some(free),
            jsc::ffi::jsc_value_get_type(),
        );
        glib::translate::from_glib_full(value)
    }
}

/// Runs `code`, logging what it throws; the source name shows in the log.
pub fn run(ctx: &jsc::Context, code: &str, source: &str) -> Option<jsc::Value> {
    let v = ctx.evaluate_with_source_uri(code, source, 1);
    if let Some(e) = ctx.exception() {
        tracing::warn!(
            source,
            line = e.line_number(),
            message = %e.message().unwrap_or_default(),
            "script error"
        );
        ctx.clear_exception();
        return None;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn rust_functions_and_promises() {
        std::thread::spawn(|| {
            let ctx = jsc::Context::new();
            let seen = Rc::new(RefCell::new(Vec::new()));
            let s = seen.clone();
            let f = function(&ctx, "note", move |args| {
                s.borrow_mut().push(args.iter().map(|a| a.to_str().to_string()).collect::<Vec<_>>().join(","));
                Some(jsc::Value::new_number(&jsc::Context::current().unwrap(), 42.0))
            });
            ctx.set_value("note", &f);
            let v = run(&ctx, "note('a', 1); Promise.resolve(2).then(x => note('then', x)); note() + 1", "test.js").unwrap();
            assert_eq!(v.to_int32(), 43);
            assert!(run(&ctx, "throw new Error('no')", "test.js").is_none());
            assert_eq!(*seen.borrow(), vec!["a,1".to_string(), String::new(), "then,2".to_string()]);
        })
        .join()
        .unwrap();
    }
}
