//! # High-level bindings to quickjs
//!
//! The `rquickjs` crate provides safe high-level bindings to the [quickjs](https://bellard.org/quickjs/) javascript engine.
//! This crate is heavily inspired by the [rlua](https://crates.io/crates/rlua) crate.
//!
//! # The `Runtime` and `Context` objects
//!
//! The main entry point of this library is the [`Runtime`] struct.
//! It represents the interperter state and is used to create [`Context`]
//! objects. As the quickjs library does not support threading the runtime is locked behind a
//! mutex. Multiple threads cannot run as script or create objects from the same runtime at the
//! same time.
//! The [`Context`] object represents a global environment and a stack. Contexts of the same runtime
//! can share javascript objects like in browser between frames of the same origin.
//!
//! # Converting Values
//!
//! This library has multiple traits for converting to and from javascript.
//! The [`IntoJs`], [`IntoJsArgs`] traits are used for taking rust values
//! and turning them into javascript values.
//! [`IntoJsArgs`] is specificly used for place where a specific number of values
//! need to be converted to javascript like for example the arguments of functions.
//! [`FromJs`] is for converting javascript value to rust.
//! Note that this trait does some automatic coercion.
//! For values which represent the name of variables or indecies the
//! trait [`IntoAtom`] is available to convert values to the represention
//! quickjs requires.
//!
//!
//! [`Runtime`]: struct.Runtime.html
//! [`Context`]: struct.Context.html
//! [`IntoJs`]: trait.IntoJs.html
//! [`IntoJsMulti`]: trait.IntoJsMulti.html
//! [`FromJs`]: trait.FromJs.html
//! [`IntoAtom`]: trait.IntoAtom.html

#![allow(clippy::needless_lifetimes)]

use quick_error::quick_error;
use std::{
    ffi::{CString, NulError},
    io, str,
};

mod context;
mod registery_key;
pub use registery_key::RegisteryKey;
mod runtime;
mod safe_ref;
pub use context::{Context, ContextBuilder, Ctx, MultiWith};
pub use runtime::Runtime;
mod markers;
mod value;
pub use markers::SendWhenParallel;
use std::result::Result as StdResult;
use std::string::String as StdString;
pub use value::*;

#[doc(hidden)]
pub use rquickjs_sys as qjs;

#[cfg(feature = "futures")]
mod promise;

#[cfg(feature = "futures")]
pub use promise::{Promise, PromiseJs};

#[cfg(feature = "allocator")]
mod allocator;

#[cfg(feature = "allocator")]
pub use allocator::{Allocator, RawMemPtr};

#[cfg(feature = "loader")]
mod loader;

#[cfg(feature = "loader")]
pub use loader::{FileResolver, Loader, Resolver, ScriptLoader};

#[cfg(feature = "dyn-load")]
pub use loader::NativeLoader;

quick_error! {
    /// Error type of the library.
    #[derive(Debug)]
    pub enum Error{
        /// Could not allocate memory
        /// This is generally only triggered when out of memory.
        Allocation{
            display("Allocation failed while creating object")
        }
        /// Found a string with a internal null byte while converting
        /// to C string.
        InvalidString(e: NulError){
            display("string contained internal null bytes: {}",e)
            from()
            cause(e)
        }
        /// String from rquickjs was not UTF-8
        Utf8(e: str::Utf8Error){
            display("Conversion from string failed: {}",e)
            from()
            cause(e)
        }
        /// An error from quickjs from which the specifics are unknown.
        /// Should eventually be removed as development progresses.
        Unknown{
            display("quickjs library created a unknown error")
        }
        /// An exception raised by quickjs itself.
        Exception{message: StdString, file: StdString, line: i32, stack: StdString}{
            display("exception generated by quickjs: [{}]:{} {}\n{}", file, line, message,stack)
        }
        /// Error converting from javascript to a rust type.
        FromJs{from: &'static str, to: &'static str, message: Option<StdString>} {
            display("error converting from js from type '{}', to '{}': {}",from,to,message.as_ref().map(|s| s.as_str()).unwrap_or(""))
        }
        /// Error converting to javascript from a rust type.
        IntoJs{from: &'static str, to: &'static str, message: Option<StdString>} {
            display("error converting from type '{}', to '{}': {}",from,to,message.as_ref().map(|s| s.as_str()).unwrap_or(""))
        }
        /// An io error
        IO(e: io::Error){
            display("IO Error: {}",e)
            from()
            cause(e)
        }
    }
}

impl Error {
    /// Returns whether the error is a quickjs generated exception.
    pub fn is_exception(&self) -> bool {
        matches!(*self, Error::Exception{..})
    }

    /// Optimized conversion to CString
    pub(crate) fn to_cstring(&self) -> CString {
        // stringify error with NUL at end
        let mut message = format!("{}\0", self).into_bytes();

        message.pop(); // pop last NUL because CString add this later

        // TODO: Replace by `CString::from_vec_with_nul_unchecked` later when it will be stabilized
        unsafe { CString::from_vec_unchecked(message) }
    }

    /// Throw an exception
    pub(crate) fn throw(&self, ctx: Ctx) -> qjs::JSValue {
        use Error::*;
        match self {
            Allocation => unsafe { qjs::JS_ThrowOutOfMemory(ctx.ctx) },
            InvalidString(_) | Utf8(_) | FromJs { .. } | IntoJs { .. } => {
                let message = self.to_cstring();
                unsafe { qjs::JS_ThrowTypeError(ctx.ctx, message.as_ptr()) }
            }
            Unknown => {
                let message = self.to_cstring();
                unsafe { qjs::JS_ThrowInternalError(ctx.ctx, message.as_ptr()) }
            }
            _ => {
                let value = self.into_js(ctx).unwrap();
                unsafe { qjs::JS_Throw(ctx.ctx, value.into_js_value()) }
            }
        }
    }
}

impl<'js> FromJs<'js> for Error {
    fn from_js(ctx: Ctx<'js>, value: Value<'js>) -> Result<Self> {
        let obj = Object::from_js(ctx, value)?;
        if obj.is_error() {
            Ok(Error::Exception {
                message: obj.get("message").unwrap_or_else(|_| "".into()),
                file: obj.get("fileName").unwrap_or_else(|_| "".into()),
                line: obj.get("lineNumber").unwrap_or(-1),
                stack: obj.get("stack").unwrap_or_else(|_| "".into()),
            })
        } else {
            Err(Error::FromJs {
                from: "object",
                to: "error",
                message: None,
            })
        }
    }
}

impl<'js> IntoJs<'js> for &Error {
    fn into_js(self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        use Error::*;
        let value = unsafe { Value::from_js_value(ctx, qjs::JS_NewError(ctx.ctx)) }?;
        if let Value::Object(obj) = &value {
            match self {
                Exception {
                    message,
                    file,
                    line,
                    stack,
                } => {
                    if !message.is_empty() {
                        obj.set("message", message)?;
                    }
                    if !file.is_empty() {
                        obj.set("fileName", file)?;
                    }
                    if *line >= 0 {
                        obj.set("lineNumber", *line)?;
                    }
                    if !stack.is_empty() {
                        obj.set("stack", stack)?;
                    }
                }
                error => {
                    obj.set("message", error.to_string())?;
                }
            }
        }
        Ok(value)
    }
}

/// Result type used throught the library.
pub type Result<T> = StdResult<T, Error>;

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn base_runtime() {
        let _rt = Runtime::new().unwrap();
    }
}
