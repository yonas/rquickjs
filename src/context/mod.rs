use crate::{
    markers::Invariant,
    runtime,
    value::{self, String},
    Error, FromJs, Module, Object, Result, Runtime, Value,
};
use rquickjs_sys as qjs;
use std::{
    ffi::{CStr, CString},
    marker::PhantomData,
    mem,
    sync::{Arc, Mutex},
};

mod builder;
pub use builder::ContextBuilder;

/// A single execution context with its own global variables and stack
/// Can share objects with other contexts of the same runtime
#[derive(Debug)]
pub struct Context {
    //TODO replace with NotNull?
    pub(crate) ctx: *mut qjs::JSContext,
    rt: Arc<Mutex<runtime::Inner>>,
}

/// A context in use, passed to [`Context::with`](struct.Context.html#method.with).
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'js> {
    pub(crate) ctx: *mut qjs::JSContext,
    marker: Invariant<'js>,
}

impl Context {
    /// Creates a base context with only the required functions registered
    /// If additional functions are required use [`Context::build`](#method.build) or [`Contex::full`](#method.full)
    pub fn base(runtime: &Runtime) -> Result<Self> {
        let guard = runtime.inner.lock().unwrap();
        let ctx = unsafe { qjs::JS_NewContextRaw(guard.rt) };
        if ctx.is_null() {
            return Err(Error::Allocation);
        }
        let res = Ok(Context {
            ctx,
            rt: runtime.inner.clone(),
        });
        res
    }

    /// Creates a context with all standart available functions registered
    /// If precise controll is required of wich functions are availble use
    /// [`Context::build`](#method.context)
    pub fn full(runtime: &Runtime) -> Result<Self> {
        let guard = runtime.inner.lock().unwrap();
        let ctx = unsafe { qjs::JS_NewContext(guard.rt) };
        if ctx.is_null() {
            return Err(Error::Allocation);
        }
        let res = Ok(Context {
            ctx,
            rt: runtime.inner.clone(),
        });
        // Explicitly drop the guard to ensure it is valid during the entire use of runtime
        mem::drop(guard);
        res
    }

    /// Create a context builder for creating a context with a specific set of intrinsics
    pub fn build(rt: &Runtime) -> ContextBuilder {
        ContextBuilder::new(rt)
    }

    /// Set the maximum stack size for the local context stack
    pub fn set_max_stack_size(&self, size: usize) {
        let guard = self.rt.lock().unwrap();
        unsafe { qjs::JS_SetMaxStackSize(self.ctx, size as u64) };
        // Explicitly drop the guard to ensure it is valid during the entire use of runtime
        mem::drop(guard)
    }

    pub fn enable_big_num_ext(&self, enable: bool) {
        let guard = self.rt.lock().unwrap();
        unsafe { qjs::JS_EnableBignumExt(self.ctx, if enable { 1 } else { 0 }) }
        // Explicitly drop the guard to ensure it is valid during the entire use of runtime
        mem::drop(guard)
    }

    /// A entry point for manipulating and using javascript objects and scripts.
    /// The api is structured this way to avoid objects being used with other runtimes.
    /// This is the only way to get a [`Ctx`](struct.Ctx.html) object.
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(Ctx) -> R,
    {
        let guard = self.rt.lock().unwrap();
        let ctx = Ctx::new(self);
        let res = f(ctx);
        // Explicitly drop the guard to ensure it is valid during the entire use of runtime
        mem::drop(guard);
        res
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let guard = self.rt.lock().unwrap();
        unsafe { qjs::JS_FreeContext(self.ctx) }
        // Explicitly drop the guard to ensure it is valid during the entire use of runtime
        mem::drop(guard)
    }
}

unsafe impl Send for Context {}

impl<'js> Ctx<'js> {
    fn new(ctx: &'js Context) -> Self {
        Ctx {
            ctx: ctx.ctx,
            marker: PhantomData,
        }
    }

    unsafe fn _eval<S: Into<Vec<u8>>>(
        self,
        source: S,
        file_name: &CStr,
        flag: i32,
    ) -> Result<qjs::JSValue> {
        let src = source.into();
        let len = src.len();
        let src = CString::new(src)?;
        let val = qjs::JS_Eval(self.ctx, src.as_ptr(), len as u64, file_name.as_ptr(), flag);
        value::handle_exception(self, val)?;
        Ok(val)
    }

    /// Evaluate a script in global context
    pub fn eval<V: FromJs<'js>, S: Into<Vec<u8>>>(self, source: S) -> Result<V> {
        let file_name = CStr::from_bytes_with_nul(b"eval_script\0").unwrap();
        let flag = qjs::JS_EVAL_TYPE_GLOBAL | qjs::JS_EVAL_FLAG_STRICT;
        unsafe {
            let val = self._eval(source, file_name, flag as i32)?;
            let val = Value::from_js_value(self, val)?;
            V::from_js(self, val)
        }
    }

    /// Compile a module for later use.
    pub fn compile<Sa, Sb>(self, source: Sa, name: Sb) -> Result<Module<'js>>
    where
        Sa: Into<Vec<u8>>,
        Sb: Into<Vec<u8>>,
    {
        let name = CString::new(name)?;
        let flag =
            qjs::JS_EVAL_TYPE_MODULE | qjs::JS_EVAL_FLAG_STRICT | qjs::JS_EVAL_FLAG_COMPILE_ONLY;
        unsafe {
            let js_val = self._eval(source, name.as_c_str(), flag as i32)?;
            Ok(Module::from_js_value(self, js_val))
        }
    }

    pub fn coerce_string(self, v: Value<'js>) -> Result<String<'js>> {
        unsafe {
            let js_val = qjs::JS_ToString(self.ctx, v.as_js_value());
            value::handle_exception(self, js_val)?;
            // js_val should be a string now
            // String itself will check for the tag when debug_assertions are enabled
            // but is should always be string
            Ok(String::from_js_value(self, js_val))
        }
    }

    pub fn coerce_i32(self, v: Value<'js>) -> Result<i32> {
        unsafe {
            let mut val: i32 = 0;
            if qjs::JS_ToInt32(self.ctx, &mut val, v.as_js_value()) < 0 {
                return Err(value::get_exception(self));
            }
            Ok(val)
        }
    }

    pub fn coerce_i64(self, v: Value<'js>) -> Result<i64> {
        unsafe {
            let mut val: i64 = 0;
            if qjs::JS_ToInt64(self.ctx, &mut val, v.as_js_value()) < 0 {
                return Err(value::get_exception(self));
            }
            Ok(val)
        }
    }

    pub fn coerce_u64(self, v: Value<'js>) -> Result<u64> {
        unsafe {
            let mut val: u64 = 0;
            if qjs::JS_ToIndex(self.ctx, &mut val, v.as_js_value()) < 0 {
                return Err(value::get_exception(self));
            }
            Ok(val)
        }
    }

    pub fn coerce_f64(self, v: Value<'js>) -> Result<f64> {
        unsafe {
            let mut val: f64 = 0.0;
            if qjs::JS_ToFloat64(self.ctx, &mut val, v.as_js_value()) < 0 {
                return Err(value::get_exception(self));
            }
            Ok(val)
        }
    }

    pub fn coerce_bool(self, v: Value<'js>) -> Result<bool> {
        unsafe {
            let val = qjs::JS_ToBool(self.ctx, v.as_js_value());
            if val < 0 {
                return Err(value::get_exception(self));
            }
            Ok(val == 1)
        }
    }

    /// Get the global object of this context
    pub fn globals(self) -> Object<'js> {
        unsafe {
            let v = qjs::JS_GetGlobalObject(self.ctx);
            Object::from_js_value(self, v)
        }
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn base() {
        let rt = Runtime::new().unwrap();
        let ctx = Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            let val = ctx.eval::<Value, _>(r#"1+1"#);
            assert_eq!(val, Ok(Value::Int(2)));
            println!("{:?}", ctx.globals());
        });
    }

    #[test]
    fn module() {
        let rt = Runtime::new().unwrap();
        let ctx = Context::full(&rt).unwrap();
        ctx.with(|ctx| {
            let value: Module = ctx
                .compile(
                    r#"
                    let t = "3";
                    let b = (a) => a + 3;
                    export { b, t}
                "#,
                    "test_mod",
                )
                .unwrap();
            println!("Value found {:?}", value);
        });
    }
}
