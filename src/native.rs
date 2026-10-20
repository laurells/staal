// Native code for pure functions (roadmap phase 5), through Cranelift.
//
// A function is compiled when everything it does can be done without effects: its
// parameters and result are Int, Bool, Text, Money, records of these, or lists of
// these, enums whose variants hold these; its body uses `let`, `set`, `if`, `match`,
// `repeat`, `loop`, `for`, `break`, `continue`, `return`, `fail`, arithmetic,
// comparisons, `and`/`or`/`not`, field reads, record, variant and list literals,
// indexing, the pure built-in functions, and calls to other compiled functions. Every other function runs on the interpreter, and a compiled function is
// only ever entered from it. Model calls, actions, `try`, `parse`/`validate`, HTTP,
// files and the journal therefore never run in native code.
//
// The interpreter is the reference, and native code reproduces it exactly:
//   - steps: one per statement and one per loop iteration, against the same limit,
//     on the same counter (passed in and out through `Ctx`);
//   - depth: checked before each call, with the same limit;
//   - Int and Bool: computed in machine code, every operation checked;
//   - every other value lives in an arena in `Ctx` and is held by native code as a
//     handle; every operation on it calls the interpreter's own function (`arith`,
//     `compare`, `builtin`, `list_index`), so results and failure messages are the
//     interpreter's, byte for byte;
//   - evaluation order: left to right, `and`/`or` short-circuit.
//
// Values: Int and Bool are i64 (Bool is 0 or 1). Anything else is a handle: an i64
// naming one arena slot. A handle has exactly one owner, as a value in the
// interpreter does: reading a variable clones its value into a new handle; helpers
// consume the handles they are given; a variable's value is dropped when the variable
// is reassigned or leaves scope (at the end of its block, a `break`, or a `return`).
// So a loop holds no more values than the interpreter would. After a failure the
// remaining handles are not dropped one by one: the whole arena is freed when native
// code returns to the interpreter, which it does at once.
//
// Calling convention: each function takes `*mut Ctx` and its arguments as i64 and
// returns i64. On failure it records the failure in `Ctx`, sets `failed`, and returns
// 0; every caller checks `failed` after each call or fallible helper, and returns.

use crate::ast::{ArithOp, Builtin, CmpOp, Expr, FnDecl, LogicOp, Pattern, Program, Stmt, Ty, Variant};
use crate::interp::value::{from_json, to_json, Value};
use serde_json::{json, Value as Json};
use crate::interp::{arith, arith_failure, builtin, compare, depth_limit, list_index, neg, step_limit, Failure, FailKind};
use cranelift_codegen::ir::{
    self, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, UserFuncName,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};

/// The state native code shares with the interpreter. The first five fields are read
/// and written by generated code at fixed offsets; the rest only by Rust.
#[repr(C)]
pub struct Ctx {
    pub steps: u64,
    pub max_steps: u64,
    pub depth: u64,
    pub max_depth: u64,
    pub failed: u64,
    pub failure: Option<Failure>,
    arena: Vec<Option<Value>>,
    free: Vec<usize>,
    tables: *const Tables,
    /// The most values alive in the arena at once during the last call.
    pub peak: usize,
}

impl Ctx {
    pub fn new(steps: u64, max_steps: u64, depth: u64, max_depth: u64) -> Ctx {
        Ctx {
            steps,
            max_steps,
            depth,
            max_depth,
            failed: 0,
            failure: None,
            arena: Vec::new(),
            free: Vec::new(),
            tables: std::ptr::null(),
            peak: 0,
        }
    }

    fn put(&mut self, v: Value) -> i64 {
        let h = match self.free.pop() {
            Some(i) => {
                self.arena[i] = Some(v);
                i as i64 + 1
            }
            None => {
                self.arena.push(Some(v));
                self.arena.len() as i64
            }
        };
        self.peak = self.peak.max(self.arena.len() - self.free.len());
        h
    }

    fn slot(&self, h: i64) -> Option<usize> {
        let i = usize::try_from(h.checked_sub(1)?).ok()?;
        (i < self.arena.len() && self.arena[i].is_some()).then_some(i)
    }

    fn take(&mut self, h: i64) -> Result<Value, Failure> {
        let i = self.slot(h).ok_or_else(|| bad_handle(h))?;
        self.free.push(i);
        Ok(self.arena[i].take().expect("checked"))
    }

    fn get(&self, h: i64) -> Result<&Value, Failure> {
        let i = self.slot(h).ok_or_else(|| bad_handle(h))?;
        Ok(self.arena[i].as_ref().expect("checked"))
    }
}

fn bad_handle(h: i64) -> Failure {
    Failure { raised: None, kind: FailKind::Internal, message: format!("native code used an invalid value handle {h}") }
}

const OFF_STEPS: i32 = 0;
const OFF_MAX_STEPS: i32 = 8;
const OFF_DEPTH: i32 = 16;
const OFF_MAX_DEPTH: i32 = 24;
const OFF_FAILED: i32 = 32;

/// Constants and names the generated code refers to by number.
#[derive(Default)]
struct Tables {
    consts: Vec<Value>,
    /// Record type name and its field names in declaration order.
    records: Vec<(String, Vec<String>)>,
    /// Field names, for field reads.
    fields: Vec<String>,
    /// Enum type name, variant name and its field names in declaration order, for
    /// variant literals.
    variants: Vec<(String, String, Vec<String>)>,
    /// Enum type name and its variant names in declaration order, for `match`.
    enums: Vec<(String, Vec<String>)>,
    /// Every compiled function, for calls of function values: its entry wrapper's
    /// address, its parameter types and its result type. Filled in once the code is in
    /// place.
    funcs: HashMap<String, (usize, Vec<Ty>, Ty)>,
}

/// The built-in functions by the number generated code passes for them.
const BUILTINS: [Builtin; 56] = [
    Builtin::Len,
    Builtin::Contains,
    Builtin::StartsWith,
    Builtin::EndsWith,
    Builtin::Header,
    Builtin::Json,
    Builtin::FillSecret,
    Builtin::Replace,
    Builtin::Lower,
    Builtin::Trim,
    Builtin::Form,
    Builtin::MinorUnits,
    Builtin::Currency,
    Builtin::UrlSegment,
    Builtin::Text,
    Builtin::Get,
    Builtin::Put,
    Builtin::Remove,
    Builtin::Keys,
    Builtin::Values,
    Builtin::Entries,
    Builtin::Add,
    Builtin::Items,
    Builtin::ToSet,
    Builtin::Upper,
    Builtin::Slice,
    Builtin::IndexOf,
    Builtin::Split,
    Builtin::Join,
    Builtin::ToInt,
    Builtin::Abs,
    Builtin::Min,
    Builtin::Max,
    Builtin::Rem,
    Builtin::Append,
    Builtin::Reverse,
    Builtin::Sort,
    Builtin::Sum,
    Builtin::Concat,
    Builtin::Map,
    Builtin::Filter,
    Builtin::Any,
    Builtin::All,
    Builtin::Count,
    Builtin::Fold,
    Builtin::Sha256,
    Builtin::Base64Encode,
    Builtin::Base64Decode,
    Builtin::HexEncode,
    Builtin::HexDecode,
    Builtin::FormatTime,
    Builtin::ParseTime,
    Builtin::MakeTime,
    Builtin::DateParts,
    Builtin::TimeParts,
    Builtin::Weekday,
];

const CMPS: [CmpOp; 6] = [CmpOp::Eq, CmpOp::Ne, CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge];

fn arith_code(op: ArithOp) -> i64 {
    match op {
        ArithOp::Add => 0,
        ArithOp::Sub => 1,
        ArithOp::Mul => 2,
        ArithOp::Div => 3,
    }
}

fn arith_op(code: i64) -> Option<ArithOp> {
    [ArithOp::Add, ArithOp::Sub, ArithOp::Mul, ArithOp::Div].get(usize::try_from(code).ok()?).copied()
}

fn internal(message: impl Into<String>) -> Failure {
    Failure { raised: None, kind: FailKind::Internal, message: message.into() }
}

// ---------------------------------------------------------------------------------
// Runtime helpers called from native code. They never panic (a panic cannot cross
// into generated code): every failure, even an internal one, is recorded in Ctx.

/// The Ctx native code passed. `Native::call` keeps it alive and exclusively borrowed
/// for the whole call, and native code makes no other reference to it.
fn ctx<'a>(c: *mut Ctx) -> &'a mut Ctx {
    // SAFETY: see above.
    unsafe { &mut *c }
}

fn record(c: &mut Ctx, f: Failure) {
    c.failure = Some(f);
    c.failed = 1;
}

/// Runs a helper body; a failure is recorded and 0 returned.
fn guard(c: *mut Ctx, f: impl FnOnce(&mut Ctx) -> Result<i64, Failure>) -> i64 {
    let c = ctx(c);
    match f(c) {
        Ok(v) => v,
        Err(e) => {
            record(c, e);
            0
        }
    }
}

fn tables<'a>(c: &Ctx) -> Result<&'a Tables, Failure> {
    if c.tables.is_null() {
        return Err(internal("native code ran without its tables"));
    }
    // SAFETY: `Native::call` points `tables` at the `Tables` it owns before running
    // any code, and they outlive the call.
    Ok(unsafe { &*c.tables })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_step_limit")]
extern "C" fn rt_step_limit(c: *mut Ctx) {
    let c = ctx(c);
    let f = step_limit(c.max_steps);
    record(c, f);
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_depth_limit")]
extern "C" fn rt_depth_limit(c: *mut Ctx) {
    let c = ctx(c);
    let f = depth_limit(c.max_depth);
    record(c, f);
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_arith")]
extern "C" fn rt_arith(c: *mut Ctx, op: i64, a: i64, b: i64) {
    let f = match arith_op(op) {
        Some(op) => arith_failure(op, a, b),
        None => internal(format!("native code: unknown operator {op}")),
    };
    record(ctx(c), f);
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_neg")]
extern "C" fn rt_neg(c: *mut Ctx, a: i64) {
    let f = match neg(a) {
        Err(f) => f,
        Ok(_) => internal(format!("native code: -({a}) reported as an overflow")),
    };
    record(ctx(c), f);
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_no_return")]
extern "C" fn rt_no_return(c: *mut Ctx) {
    record(ctx(c), internal("a native function finished without returning a value"));
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_const")]
extern "C" fn rt_const(c: *mut Ctx, idx: i64) -> i64 {
    guard(c, |c| {
        let v = usize::try_from(idx).ok().and_then(|i| tables(c).ok()?.consts.get(i)).cloned();
        let v = v.ok_or_else(|| internal(format!("native code: no constant {idx}")))?;
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_clone")]
extern "C" fn rt_clone(c: *mut Ctx, h: i64) -> i64 {
    guard(c, |c| {
        let v = c.get(h)?.clone();
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_drop")]
extern "C" fn rt_drop(c: *mut Ctx, h: i64) {
    guard(c, |c| c.take(h).map(|_| 0));
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_box")]
extern "C" fn rt_box(c: *mut Ctx, kind: i64, v: i64) -> i64 {
    guard(c, |c| Ok(c.put(if kind == 0 { Value::Int(v) } else { Value::Bool(v != 0) })))
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_unbox")]
extern "C" fn rt_unbox(c: *mut Ctx, h: i64) -> i64 {
    guard(c, |c| match c.take(h)? {
        Value::Int(n) => Ok(n),
        Value::Bool(b) => Ok(b as i64),
        _ => Err(internal("native code: expected an Int or Bool")),
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_arith_val")]
extern "C" fn rt_arith_val(c: *mut Ctx, op: i64, a: i64, b: i64) -> i64 {
    guard(c, |c| {
        let op = arith_op(op).ok_or_else(|| internal("native code: unknown operator"))?;
        let (l, r) = (c.take(a)?, c.take(b)?);
        let v = arith(op, l, r)?;
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_cmp")]
extern "C" fn rt_cmp(c: *mut Ctx, op: i64, a: i64, b: i64) -> i64 {
    guard(c, |c| {
        let op = usize::try_from(op).ok().and_then(|i| CMPS.get(i)).copied();
        let op = op.ok_or_else(|| internal("native code: unknown comparison"))?;
        let (l, r) = (c.take(a)?, c.take(b)?);
        Ok(compare(op, &l, &r)? as i64)
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_closure")]
extern "C" fn rt_closure(c: *mut Ctx, name: i64, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let name = match usize::try_from(name).ok().and_then(|i| tables(c).ok()?.consts.get(i)) {
            Some(Value::Text(t)) => t.clone(),
            _ => return Err(internal(format!("native code: no function name {name}"))),
        };
        let captured = take_args(c, argv, n)?;
        Ok(c.put(Value::Func { name, captured }))
    })
}

/// A call of the function value `fh` with the `n` arguments in `argv`: the arguments
/// its function takes first are what it captured.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_call_value")]
extern "C" fn rt_call_value(c: *mut Ctx, fh: i64, argv: *const i64, n: i64) -> i64 {
    let prepared = (|| -> Result<(usize, Vec<i64>), Failure> {
        let cx = ctx(c);
        let Value::Func { name, captured } = cx.take(fh)? else {
            return Err(internal("native code: calling a value that is not a function"));
        };
        let (ptr, params, _) = tables(cx)?
            .funcs
            .get(&name)
            .cloned()
            .ok_or_else(|| internal(format!("native code: the function value '{name}' is not compiled")))?;
        let n = usize::try_from(n).map_err(|_| internal("native code: a negative argument count"))?;
        if params.len() != captured.len() + n {
            return Err(internal(format!("native code: '{name}' called with {} arguments, expects {}", captured.len() + n, params.len())));
        }
        let mut all = Vec::with_capacity(params.len());
        for (v, t) in captured.into_iter().zip(&params) {
            match (v, rep(t)) {
                (Value::Int(i), Rep::Raw) if is(t, "Int") => all.push(i),
                (Value::Bool(b), Rep::Raw) if is(t, "Bool") => all.push(b as i64),
                (_, Rep::Raw) => return Err(internal(format!("native code: a captured value of '{name}' is not a {t}"))),
                (v, Rep::Handle) => all.push(cx.put(v)),
            }
        }
        for i in 0..n {
            // SAFETY: generated code passes a stack array of exactly `n` i64 values.
            all.push(unsafe { *argv.add(i) });
        }
        Ok((ptr, all))
    })();
    match prepared {
        Err(e) => {
            record(ctx(c), e);
            0
        }
        Ok((ptr, all)) => {
            // SAFETY: `ptr` is the entry wrapper of a compiled function, with signature
            // (ctx, argv) -> i64, from the tables of the code running now; `all` holds
            // exactly as many i64 as it takes. No reference to the Ctx is held here.
            let f = unsafe { std::mem::transmute::<usize, extern "C" fn(*mut Ctx, *const i64) -> i64>(ptr) };
            f(c, all.as_ptr())
        }
    }
}

/// Calls function value `func` with `args` after what it captured, as a call in
/// generated code does: the depth check, then depth + 1 around the call. Err(None)
/// when the function failed, which it has recorded in the Ctx itself.
fn call_function(c: *mut Ctx, func: &Value, args: Vec<Value>) -> Result<Value, Option<Failure>> {
    let Value::Func { name, captured } = func else {
        return Err(Some(internal("native code: calling a value that is not a function")));
    };
    let (ptr, params, ret) = {
        let cx = ctx(c);
        let found = tables(cx).map_err(Some)?.funcs.get(name).cloned();
        let found = found.ok_or_else(|| Some(internal(format!("native code: the function value '{name}' is not compiled"))))?;
        if cx.depth >= cx.max_depth {
            return Err(Some(depth_limit(cx.max_depth)));
        }
        found
    };
    if params.len() != captured.len() + args.len() {
        return Err(Some(internal(format!("native code: '{name}' called with {} arguments, expects {}", captured.len() + args.len(), params.len()))));
    }
    let mut all = Vec::with_capacity(params.len());
    {
        let cx = ctx(c);
        for (v, t) in captured.iter().cloned().chain(args).zip(&params) {
            match (v, rep(t)) {
                (Value::Int(i), Rep::Raw) if is(t, "Int") => all.push(i),
                (Value::Bool(b), Rep::Raw) if is(t, "Bool") => all.push(b as i64),
                (_, Rep::Raw) => return Err(Some(internal(format!("native code: an argument of '{name}' is not a {t}")))),
                (v, Rep::Handle) => all.push(cx.put(v)),
            }
        }
        cx.depth += 1;
    }
    // SAFETY: `ptr` is the entry wrapper of a compiled function, with signature
    // (ctx, argv) -> i64, from the tables of the code running now; `all` holds exactly
    // as many i64 as it takes. No reference to the Ctx is held across the call.
    let f = unsafe { std::mem::transmute::<usize, extern "C" fn(*mut Ctx, *const i64) -> i64>(ptr) };
    let r = f(c, all.as_ptr());
    let cx = ctx(c);
    cx.depth -= 1;
    if cx.failed != 0 {
        return Err(None);
    }
    match rep(&ret) {
        Rep::Raw if is(&ret, "Int") => Ok(Value::Int(r)),
        Rep::Raw => Ok(Value::Bool(r != 0)),
        Rep::Handle => cx.take(r).map_err(Some),
    }
}

/// `map`, `filter`, `any`, `all`, `count` and `fold` (built-in `code`), with the list,
/// the start for `fold`, and the function value in `argv`: as the interpreter does.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_with_function")]
extern "C" fn rt_with_function(c: *mut Ctx, code: i64, argv: *const i64, n: i64) -> i64 {
    let run = || -> Result<Value, Option<Failure>> {
        let f = usize::try_from(code).ok().and_then(|i| BUILTINS.get(i)).copied();
        let f = f.ok_or_else(|| Some(internal("native code: unknown built-in")))?;
        let mut it = take_args(ctx(c), argv, n).map_err(Some)?.into_iter();
        let (Some(Value::List(xs)), start) = (it.next(), if f == Builtin::Fold { it.next() } else { None }) else {
            return Err(Some(internal(format!("native code: `{}` of a value that is not a list", f.name()))));
        };
        let Some(func) = it.next() else { return Err(Some(internal("native code: no function value"))) };
        let test = |v: Value| match v {
            Value::Bool(b) => Ok(b),
            _ => Err(Some(internal(format!("native code: `{}` given a function that did not give a Bool", f.name())))),
        };
        match f {
            Builtin::Map => {
                let mut out = Vec::with_capacity(xs.len());
                for x in xs {
                    out.push(call_function(c, &func, vec![x])?);
                }
                Ok(Value::List(out))
            }
            Builtin::Filter => {
                let mut out = Vec::new();
                for x in xs {
                    if test(call_function(c, &func, vec![x.clone()])?)? {
                        out.push(x);
                    }
                }
                Ok(Value::List(out))
            }
            Builtin::Any | Builtin::All => {
                let want = f == Builtin::Any;
                for x in xs {
                    if test(call_function(c, &func, vec![x])?)? == want {
                        return Ok(Value::Bool(want));
                    }
                }
                Ok(Value::Bool(!want))
            }
            Builtin::Count => {
                let mut k: i64 = 0;
                for x in xs {
                    if test(call_function(c, &func, vec![x])?)? {
                        k += 1;
                    }
                }
                Ok(Value::Int(k))
            }
            Builtin::Fold => {
                let Some(mut acc) = start else { return Err(Some(internal("native code: `fold` without a start"))) };
                for x in xs {
                    acc = call_function(c, &func, vec![acc, x])?;
                }
                Ok(acc)
            }
            _ => Err(Some(internal(format!("native code: `{}` does not call a function", f.name())))),
        }
    };
    match run() {
        Ok(v) => ctx(c).put(v),
        Err(Some(e)) => {
            record(ctx(c), e);
            0
        }
        // The function failed and recorded why.
        Err(None) => 0,
    }
}

/// Reads `n` handles from `argv` and takes their values.
fn take_args(c: &mut Ctx, argv: *const i64, n: i64) -> Result<Vec<Value>, Failure> {
    let n = usize::try_from(n).map_err(|_| bad_handle(n))?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // SAFETY: generated code passes a stack array of exactly `n` i64 handles.
        let h = unsafe { *argv.add(i) };
        out.push(c.take(h)?);
    }
    Ok(out)
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_builtin")]
extern "C" fn rt_builtin(c: *mut Ctx, code: i64, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let f = usize::try_from(code).ok().and_then(|i| BUILTINS.get(i)).copied();
        let f = f.ok_or_else(|| internal("native code: unknown built-in"))?;
        let args = take_args(c, argv, n)?;
        let v = builtin(f, &args)?;
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_record")]
extern "C" fn rt_record(c: *mut Ctx, idx: i64, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let t = tables(c)?;
        let rec = usize::try_from(idx).ok().and_then(|i| t.records.get(i));
        let (ty, names) = rec.ok_or_else(|| internal("native code: unknown record"))?;
        let vals = take_args(c, argv, n)?;
        if vals.len() != names.len() {
            return Err(internal(format!("native code: {ty} with {} fields", vals.len())));
        }
        let fields = names.iter().cloned().zip(vals).collect();
        Ok(c.put(Value::Record { ty: ty.clone(), fields }))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_field")]
extern "C" fn rt_field(c: *mut Ctx, h: i64, idx: i64) -> i64 {
    guard(c, |c| {
        let t = tables(c)?;
        let name = usize::try_from(idx).ok().and_then(|i| t.fields.get(i));
        let name = name.ok_or_else(|| internal("native code: unknown field"))?;
        match c.take(h)? {
            Value::Record { fields, .. } => match fields.into_iter().find(|(f, _)| f == name) {
                Some((_, v)) => Ok(c.put(v)),
                None => Err(internal(format!("no field '{name}'"))),
            },
            Value::Tuple(items) => {
                let v = crate::interp::tuple_item(items, name)?;
                Ok(c.put(v))
            }
            _ => Err(internal(format!("cannot read '.{name}' of this value"))),
        }
    })
}

/// A copy of field `idx` of the record in `h`, which stays owned by the caller: a
/// field read from a variable copies the field, not the whole record.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_field_ref")]
extern "C" fn rt_field_ref(c: *mut Ctx, h: i64, idx: i64) -> i64 {
    guard(c, |c| {
        let t = tables(c)?;
        let name = usize::try_from(idx).ok().and_then(|i| t.fields.get(i));
        let name = name.ok_or_else(|| internal("native code: unknown field"))?;
        let v = match c.get(h)? {
            // An enum's fields are read only by `match` binding them.
            Value::Record { fields, .. } | Value::Enum { fields, .. } => match fields.iter().find(|(f, _)| f == name) {
                Some((_, v)) => v.clone(),
                None => return Err(internal(format!("no field '{name}'"))),
            },
            Value::Tuple(items) => crate::interp::tuple_item(items.clone(), name)?,
            _ => return Err(internal(format!("cannot read '.{name}' of this value"))),
        };
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_variant")]
extern "C" fn rt_variant(c: *mut Ctx, idx: i64, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let t = tables(c)?;
        let var = usize::try_from(idx).ok().and_then(|i| t.variants.get(i));
        let (ty, variant, names) = var.ok_or_else(|| internal("native code: unknown variant"))?;
        let vals = take_args(c, argv, n)?;
        if vals.len() != names.len() {
            return Err(internal(format!("native code: {ty}.{variant} with {} fields", vals.len())));
        }
        let fields = names.iter().cloned().zip(vals).collect();
        Ok(c.put(Value::Enum { ty: ty.clone(), variant: variant.clone(), fields }))
    })
}

/// The position, among the variants of enum `idx`, of the variant of the enum value
/// in `h`, which stays owned by the caller.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_variant_index")]
extern "C" fn rt_variant_index(c: *mut Ctx, h: i64, idx: i64) -> i64 {
    guard(c, |c| {
        let t = tables(c)?;
        let e = usize::try_from(idx).ok().and_then(|i| t.enums.get(i));
        let (ty, names) = e.ok_or_else(|| internal("native code: unknown enum"))?;
        let Value::Enum { variant, .. } = c.get(h)? else { return Err(internal("`match` on a value that is not an enum")) };
        match names.iter().position(|n| n == variant) {
            Some(i) => Ok(i as i64),
            None => Err(internal(format!("native code: {variant} is not a variant of {ty}"))),
        }
    })
}

/// No arm matched the enum value in `h`: the interpreter's failure, word for word.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_no_arm")]
extern "C" fn rt_no_arm(c: *mut Ctx, h: i64) {
    guard(c, |c| {
        let variant = match c.get(h)? {
            Value::Enum { variant, .. } => variant.clone(),
            _ => return Err(internal("`match` on a value that is not an enum")),
        };
        Err(internal(format!("no `match` arm for variant {variant}")))
    });
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_none")]
extern "C" fn rt_none(c: *mut Ctx) -> i64 {
    guard(c, |c| Ok(c.put(Value::Opt(None))))
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_some")]
extern "C" fn rt_some(c: *mut Ctx, h: i64) -> i64 {
    guard(c, |c| {
        let v = c.take(h)?;
        Ok(c.put(Value::Opt(Some(Box::new(v)))))
    })
}

/// Whether the optional value in `h`, which stays owned by the caller, holds one.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_opt_has")]
extern "C" fn rt_opt_has(c: *mut Ctx, h: i64) -> i64 {
    guard(c, |c| match c.get(h)? {
        Value::Opt(v) => Ok(v.is_some() as i64),
        _ => Err(internal("`match` on a value that is neither an enum nor optional")),
    })
}

/// A copy of what the optional value in `h` holds; `h` stays owned by the caller.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_opt_get_ref")]
extern "C" fn rt_opt_get_ref(c: *mut Ctx, h: i64) -> i64 {
    guard(c, |c| {
        let v = match c.get(h)? {
            Value::Opt(Some(v)) => (**v).clone(),
            _ => return Err(internal("native code: `some` arm on a value that holds nothing")),
        };
        Ok(c.put(v))
    })
}

/// No arm matched an optional value: the interpreter's failure, word for word.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_opt_no_arm")]
extern "C" fn rt_opt_no_arm(c: *mut Ctx) {
    record(ctx(c), internal("no `match` arm for this optional value"));
}

/// Item `i` of the list in `h`, which stays owned by the caller.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_index_ref")]
extern "C" fn rt_index_ref(c: *mut Ctx, list: i64, i: i64) -> i64 {
    guard(c, |c| {
        let v = match c.get(list)? {
            Value::List(items) => list_index(items, i)?,
            _ => return Err(internal("indexing a non-list")),
        };
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_list")]
extern "C" fn rt_list(c: *mut Ctx, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let vals = take_args(c, argv, n)?;
        Ok(c.put(Value::List(vals)))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_tuple")]
extern "C" fn rt_tuple(c: *mut Ctx, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let vals = take_args(c, argv, n)?;
        Ok(c.put(Value::Tuple(vals)))
    })
}

/// An empty list (0), set (1) or map (2).
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_empty")]
extern "C" fn rt_empty(c: *mut Ctx, kind: i64) -> i64 {
    guard(c, |c| {
        let v = match kind {
            0 => Value::List(Vec::new()),
            1 => Value::Set(Vec::new()),
            2 => Value::Map(Vec::new()),
            _ => return Err(internal(format!("native code: no empty collection of kind {kind}"))),
        };
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_map")]
extern "C" fn rt_map(c: *mut Ctx, argv: *const i64, n: i64) -> i64 {
    guard(c, |c| {
        let vals = take_args(c, argv, n)?;
        let m = crate::interp::map_literal(vals)?;
        Ok(c.put(m))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_index")]
extern "C" fn rt_index(c: *mut Ctx, list: i64, i: i64) -> i64 {
    guard(c, |c| match c.take(list)? {
        Value::List(items) => {
            let v = list_index(&items, i)?;
            Ok(c.put(v))
        }
        _ => Err(internal("indexing a non-list")),
    })
}

/// The length of a list, which stays owned by the caller.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_list_len")]
extern "C" fn rt_list_len(c: *mut Ctx, list: i64) -> i64 {
    guard(c, |c| match c.get(list)? {
        Value::List(items) => Ok(items.len() as i64),
        _ => Err(internal("`for` over a value that is not a list")),
    })
}

/// A copy of item `i` of a list, which stays owned by the caller.
// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_list_get")]
extern "C" fn rt_list_get(c: *mut Ctx, list: i64, i: i64) -> i64 {
    guard(c, |c| {
        let v = match c.get(list)? {
            Value::List(items) => usize::try_from(i).ok().and_then(|u| items.get(u)).cloned(),
            _ => None,
        };
        let v = v.ok_or_else(|| internal("native code: `for` item out of range"))?;
        Ok(c.put(v))
    })
}

// Exported so `staalc build` executables link to it by name.
#[unsafe(export_name = "staal_rt_fail")]
extern "C" fn rt_fail(c: *mut Ctx, h: i64) {
    guard(c, |c| {
        Err(match c.take(h)? {
            v @ (Value::Text(_) | Value::Enum { .. } | Value::Record { .. }) => crate::interp::raise(v),
            _ => internal("`fail` with a message that is not Text"),
        })
    });
}

// ---------------------------------------------------------------------------------
// Types: which values native code can hold, and the type of every expression.

/// How native code holds a value of a type.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Rep {
    /// Int or Bool, as an i64.
    Raw,
    /// Any other value, as an arena handle.
    Handle,
}

fn rep(t: &Ty) -> Rep {
    if is(t, "Int") || is(t, "Bool") { Rep::Raw } else { Rep::Handle }
}

fn is(t: &Ty, n: &str) -> bool {
    matches!(t, Ty::Named(x) if x == n)
}

fn named(n: &str) -> Ty {
    Ty::Named(n.into())
}

/// The program's record and enum types and compilable function signatures.
struct Env<'p> {
    records: HashMap<&'p str, &'p [(String, Ty)]>,
    enums: HashMap<&'p str, &'p [Variant]>,
    sigs: HashMap<&'p str, (Vec<Ty>, Ty)>,
}

impl<'p> Env<'p> {
    fn new(prog: &'p Program) -> Env<'p> {
        let records: HashMap<&str, &[(String, Ty)]> = prog.types.iter().map(|t| (t.name.as_str(), t.fields.as_slice())).collect();
        let enums: HashMap<&str, &[Variant]> = prog.enums.iter().map(|e| (e.name.as_str(), e.variants.as_slice())).collect();
        let mut env = Env { records, enums, sigs: HashMap::new() };
        let mut sigs = HashMap::new();
        for f in &prog.fns {
            if f.name.contains(' ') {
                continue;
            }
            let params: Option<Vec<Ty>> = f.params.iter().map(|p| env.allowed(&p.ty, 0).then(|| p.ty.clone())).collect();
            if let (Some(params), Some(ret)) = (params, f.ret.as_ref().filter(|r| env.allowed(r, 0))) {
                sigs.insert(f.name.as_str(), (params, ret.clone()));
            }
        }
        env.sigs = sigs;
        env
    }

    /// The fields of variant `variant` of enum `ty`.
    fn variant(&self, ty: &str, variant: &str) -> Option<&'p [(String, Ty)]> {
        self.enums.get(ty)?.iter().find(|v| v.name == variant).map(|v| v.fields.as_slice())
    }

    /// Values native code may hold: Int, Bool, Text, Money, and records, enums and
    /// lists of them.
    fn allowed(&self, t: &Ty, depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        match t {
            Ty::Named(n) if ["Int", "Bool", "Text", "Money"].contains(&n.as_str()) => true,
            Ty::Named(n) if self.enums.contains_key(n.as_str()) => self.enums[n.as_str()]
                .iter()
                .all(|v| v.fields.iter().all(|(_, ft)| self.allowed(ft, depth + 1))),
            Ty::Named(n) => self.records.get(n.as_str()).is_some_and(|fs| fs.iter().all(|(_, ft)| self.allowed(ft, depth + 1))),
            Ty::List(inner, _) => self.allowed(inner, depth + 1),
            // `none`'s own type, whose only value is `none`, or an optional allowed value.
            Ty::Optional(_) if *t == Ty::none() => true,
            Ty::Optional(inner) => self.allowed(inner, depth + 1),
            Ty::Tuple(items) => items.iter().all(|t| self.allowed(t, depth + 1)),
            Ty::Map(k, v, _) => self.allowed(k, depth + 1) && self.allowed(v, depth + 1),
            Ty::Set(t, _) => self.allowed(t, depth + 1),
            Ty::Func(ps, r) => ps.iter().all(|p| self.allowed(p, depth + 1)) && self.allowed(r, depth + 1),
            Ty::Proposed(_) | Ty::Cap { .. } => false,
        }
    }

    /// The type of `e`, or None if native code does not support it.
    fn ty(&self, e: &Expr, var: &dyn Fn(&str) -> Option<Ty>) -> Option<Ty> {
        let t = match e {
            Expr::Var(n, _) => var(n)?,
            Expr::Str(..) => named("Text"),
            Expr::Int(n, _) => {
                i64::try_from(*n).ok()?;
                named("Int")
            }
            Expr::Bool(..) => named("Bool"),
            Expr::Money(..) => named("Money"),
            Expr::Arith { op, left, right, .. } => {
                let (l, r) = (self.ty(left, var)?, self.ty(right, var)?);
                let money_int = (is(&l, "Money") && is(&r, "Int")) || (is(&l, "Int") && is(&r, "Money"));
                if is(&l, "Int") && is(&r, "Int") {
                    named("Int")
                } else if (is(&l, "Money") && is(&r, "Money") && matches!(op, ArithOp::Add | ArithOp::Sub))
                    || (*op == ArithOp::Mul && money_int)
                {
                    named("Money")
                } else if is(&l, "Text") && is(&r, "Text") && *op == ArithOp::Add {
                    named("Text")
                } else {
                    return None;
                }
            }
            Expr::Neg(inner, _) => {
                if !is(&self.ty(inner, var)?, "Int") {
                    return None;
                }
                named("Int")
            }
            Expr::Not(inner, _) => {
                if !is(&self.ty(inner, var)?, "Bool") {
                    return None;
                }
                named("Bool")
            }
            Expr::Logic { left, right, .. } => {
                if !(is(&self.ty(left, var)?, "Bool") && is(&self.ty(right, var)?, "Bool")) {
                    return None;
                }
                named("Bool")
            }
            Expr::Cmp { op, left, right, .. } => {
                let (l, r) = (self.ty(left, var)?, self.ty(right, var)?);
                let ordered = (is(&l, "Int") && is(&r, "Int")) || (is(&l, "Money") && is(&r, "Money"));
                if !(ordered || (matches!(op, CmpOp::Eq | CmpOp::Ne) && l.unify(&r).is_some())) {
                    return None;
                }
                named("Bool")
            }
            Expr::Field { base, field, .. } => {
                let bt = self.ty(base, var)?;
                if let Ty::Tuple(items) = &bt {
                    return field.parse::<usize>().ok().and_then(|i| items.get(i)).cloned().filter(|t| self.allowed(t, 0));
                }
                let Ty::Named(n) = bt else { return None };
                let fields = self.records.get(n.as_str())?;
                fields.iter().find(|(f, _)| f == field)?.1.clone()
            }
            Expr::Record { ty, fields, .. } => {
                let decl = self.records.get(ty.as_str())?;
                if fields.len() != decl.len() {
                    return None;
                }
                for (name, fe) in fields {
                    let want = &decl.iter().find(|(f, _)| f == name)?.1;
                    if !self.ty(fe, var)?.widens(want) {
                        return None;
                    }
                }
                named(ty)
            }
            Expr::Variant { ty, variant, fields, .. } => {
                let decl = self.variant(ty, variant)?;
                if fields.len() != decl.len() {
                    return None;
                }
                for (name, fe) in fields {
                    let want = &decl.iter().find(|(f, _)| f == name)?.1;
                    if !self.ty(fe, var)?.widens(want) {
                        return None;
                    }
                }
                named(ty)
            }
            Expr::List(items, _) => {
                let mut first = self.ty(items.first()?, var)?;
                for i in &items[1..] {
                    first = first.unify(&self.ty(i, var)?)?;
                }
                Ty::List(Box::new(first), items.len() as u64)
            }
            Expr::NoneLit(_) => Ty::none(),
            Expr::Tuple(items, _) => Ty::Tuple(items.iter().map(|i| self.ty(i, var)).collect::<Option<Vec<_>>>()?),
            Expr::MapLit(entries, _) => {
                let (k0, v0) = entries.first()?;
                let kt = self.ty(k0, var)?;
                let mut vt = self.ty(v0, var)?;
                for (k, v) in &entries[1..] {
                    if self.ty(k, var)? != kt {
                        return None;
                    }
                    vt = vt.unify(&self.ty(v, var)?)?;
                }
                Ty::Map(Box::new(kt), Box::new(vt), entries.len() as u64)
            }
            Expr::SomeOf(inner, _) => Ty::Optional(Box::new(self.ty(inner, var)?)),
            Expr::Index { base, index, .. } => {
                let Ty::List(inner, _) = self.ty(base, var)? else { return None };
                if !is(&self.ty(index, var)?, "Int") {
                    return None;
                }
                *inner
            }
            Expr::Builtin { f, args, .. } => {
                let tys: Vec<Ty> = args.iter().map(|a| self.ty(a, var)).collect::<Option<_>>()?;
                let first = tys.first();
                match f {
                    // The checker typed these; their results follow from the first argument.
                    Builtin::Get => match first? {
                        Ty::Map(_, v, _) => Ty::Optional(v.clone()),
                        _ => return None,
                    },
                    Builtin::Put | Builtin::Remove | Builtin::Add => first?.clone(),
                    Builtin::Keys => match first? {
                        Ty::Map(k, _, n) => Ty::List(k.clone(), *n),
                        _ => return None,
                    },
                    Builtin::Values => match first? {
                        Ty::Map(_, v, n) => Ty::List(v.clone(), *n),
                        _ => return None,
                    },
                    Builtin::Entries => match first? {
                        Ty::Map(k, v, n) => Ty::List(Box::new(Ty::Tuple(vec![(**k).clone(), (**v).clone()])), *n),
                        _ => return None,
                    },
                    Builtin::Items => match first? {
                        Ty::Set(t, n) => Ty::List(t.clone(), *n),
                        _ => return None,
                    },
                    Builtin::ToSet => match first? {
                        Ty::List(t, n) => Ty::Set(t.clone(), *n),
                        _ => return None,
                    },
                    Builtin::Len | Builtin::MinorUnits => named("Int"),
                    Builtin::Contains | Builtin::StartsWith | Builtin::EndsWith => named("Bool"),
                    Builtin::Json
                    | Builtin::Replace
                    | Builtin::Lower
                    | Builtin::Trim
                    | Builtin::Form
                    | Builtin::Currency
                    | Builtin::UrlSegment
                    | Builtin::Text => named("Text"),
                    // Headers and secrets belong to requests, which native code never makes.
                    Builtin::Header | Builtin::FillSecret => return None,
                    Builtin::Upper | Builtin::Join | Builtin::Sha256 | Builtin::Base64Encode | Builtin::HexEncode => named("Text"),
                    Builtin::Base64Decode | Builtin::HexDecode => Ty::Optional(Box::new(named("Text"))),
                    Builtin::FormatTime => named("Text"),
                    Builtin::ParseTime => Ty::Optional(Box::new(named("Int"))),
                    Builtin::MakeTime | Builtin::Weekday => named("Int"),
                    Builtin::DateParts => Ty::Tuple(vec![named("Int"); 3]),
                    Builtin::TimeParts => Ty::Tuple(vec![named("Int"); 4]),
                    Builtin::Slice | Builtin::Append | Builtin::Reverse | Builtin::Sort | Builtin::Min | Builtin::Max => first?.clone(),
                    Builtin::IndexOf | Builtin::ToInt => Ty::Optional(Box::new(named("Int"))),
                    Builtin::Abs | Builtin::Rem | Builtin::Sum => named("Int"),
                    // The most parts is the literal the checker required.
                    Builtin::Split => match args.get(2) {
                        Some(Expr::Int(n, _)) => Ty::List(Box::new(named("Text")), *n),
                        _ => return None,
                    },
                    Builtin::Map => match (first?, tys.get(1)?) {
                        (Ty::List(_, n), Ty::Func(_, r)) => Ty::List(r.clone(), *n),
                        _ => return None,
                    },
                    Builtin::Filter => first?.clone(),
                    Builtin::Any | Builtin::All => named("Bool"),
                    Builtin::Count => named("Int"),
                    Builtin::Fold => match tys.get(2)? {
                        Ty::Func(_, r) => (**r).clone(),
                        _ => return None,
                    },
                    Builtin::Concat => match (first?, tys.get(1)?) {
                        (Ty::List(a, n), Ty::List(b, m)) => Ty::List(Box::new(a.unify(b)?), n.checked_add(*m)?),
                        _ => return None,
                    },
                }
            }
            Expr::Call { name, args, slots, .. } => {
                let (params, ret) = self.sigs.get(name.as_str())?;
                if params.len() != args.len() {
                    return None;
                }
                for (i, a) in args.iter().enumerate() {
                    let p = params.get(slots.get(i).copied().unwrap_or(i))?;
                    if !self.ty(a, var)?.widens(p) {
                        return None;
                    }
                }
                ret.clone()
            }
            // A compiled function as a value, with what it captured as its first arguments.
            Expr::Closure { name, captures, .. } => {
                let (params, ret) = self.sigs.get(name.as_str())?;
                if captures.len() > params.len() {
                    return None;
                }
                for (c, p) in captures.iter().zip(params) {
                    if !self.ty(c, var)?.widens(p) {
                        return None;
                    }
                }
                Ty::Func(params[captures.len()..].to_vec(), Box::new(ret.clone()))
            }
            Expr::CallValue { callee, args, .. } => {
                let Ty::Func(params, ret) = self.ty(callee, var)? else { return None };
                if params.len() != args.len() {
                    return None;
                }
                for (a, p) in args.iter().zip(&params) {
                    if !self.ty(a, var)?.widens(p) {
                        return None;
                    }
                }
                *ret
            }
            _ => return None,
        };
        self.allowed(&t, 0).then_some(t)
    }
}

/// Checks a function's code, tracking the type of every variable in scope.
struct Check<'e, 'p> {
    env: &'e Env<'p>,
    scopes: Vec<HashMap<String, Ty>>,
    ret: Ty,
    /// `fails E`: what `fail(...)` may raise besides Text.
    fails: Option<Ty>,
}

impl Check<'_, '_> {
    fn lookup(&self, n: &str) -> Option<Ty> {
        self.scopes.iter().rev().find_map(|s| s.get(n).cloned())
    }

    fn ty(&self, e: &Expr) -> Option<Ty> {
        self.env.ty(e, &|n| self.lookup(n))
    }

    fn block(&mut self, body: &[Stmt]) -> bool {
        self.scopes.push(HashMap::new());
        let ok = body.iter().all(|s| self.stmt(s));
        self.scopes.pop();
        ok
    }

    fn stmt(&mut self, s: &Stmt) -> bool {
        match s {
            Stmt::Let { name, ty, value, .. } => {
                // `[]` and `[:]` take their declared list, set or map type.
                if let Some(d @ (Ty::List(..) | Ty::Set(..) | Ty::Map(..))) = ty
                    && empty_literal(value, d)
                {
                    if !self.env.allowed(d, 0) {
                        return false;
                    }
                    self.scopes.last_mut().expect("a scope").insert(name.clone(), d.clone());
                    return true;
                }
                let Some(t) = self.ty(value) else { return false };
                // A typed `let` holds its declared type (`let x: Optional<Int> = none`).
                let t = match ty {
                    Some(d) if t.widens(d) && self.env.allowed(d, 0) => d.clone(),
                    Some(_) => return false,
                    None => t,
                };
                self.scopes.last_mut().expect("a scope").insert(name.clone(), t);
                true
            }
            Stmt::Set { name, value, .. } => match (self.ty(value), self.lookup(name)) {
                (Some(v), Some(t)) => v.widens(&t),
                _ => false,
            },
            Stmt::Expr(e) => self.ty(e).is_some(),
            Stmt::Return(Some(e), _) => self.ty(e).is_some_and(|t| t.widens(&self.ret)),
            Stmt::Fail(e, _) => self.ty(e).is_some_and(|t| if let Some(f) = &self.fails { t.widens(f) } else { is(&t, "Text") }),
            Stmt::If { cond, then, els, .. } => self.ty(cond).is_some_and(|t| is(&t, "Bool")) && self.block(then) && self.block(els),
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => self.block(body),
            Stmt::For { var, list, body, .. } => {
                let Some(Ty::List(inner, _)) = self.ty(list) else { return false };
                self.scopes.push(HashMap::from([(var.clone(), *inner)]));
                let ok = self.block(body);
                self.scopes.pop();
                ok
            }
            Stmt::Match { value, arms, .. } => {
                let vt = self.ty(value);
                if let Some(Ty::Optional(inner)) = vt {
                    return arms.iter().all(|arm| {
                        let mut scope = HashMap::new();
                        if let Pattern::Present { bind, .. } = &arm.pattern {
                            scope.insert(bind.clone(), (*inner).clone());
                        }
                        self.scopes.push(scope);
                        let ok = self.block(&arm.body);
                        self.scopes.pop();
                        ok
                    });
                }
                let Some(Ty::Named(ty)) = vt else { return false };
                if !self.env.enums.contains_key(ty.as_str()) {
                    return false;
                }
                arms.iter().all(|arm| {
                    let mut scope = HashMap::new();
                    if let Pattern::Variant { name, binds, .. } = &arm.pattern {
                        let Some(decl) = self.env.variant(&ty, name) else { return false };
                        for (field, to) in binds {
                            let Some((_, ft)) = decl.iter().find(|(f, _)| f == field) else { return false };
                            scope.insert(to.clone(), ft.clone());
                        }
                    }
                    self.scopes.push(scope);
                    let ok = self.block(&arm.body);
                    self.scopes.pop();
                    ok
                })
            }
            Stmt::Break(_) | Stmt::Continue(_) => true,
            Stmt::Return(None, _) => false,
        }
    }
}

/// Whether `value` is `[]` declared as a list or set, or `[:]` declared as a map.
fn empty_literal(value: &Expr, declared: &Ty) -> bool {
    match (value, declared) {
        (Expr::List(items, _), Ty::List(..) | Ty::Set(..)) => items.is_empty(),
        (Expr::MapLit(entries, _), Ty::Map(..)) => entries.is_empty(),
        _ => false,
    }
}

/// The functions that can run as native code: those whose own code qualifies and
/// whose every callee does too.
pub fn eligible(prog: &Program) -> HashSet<String> {
    let env = Env::new(prog);
    let mut set: HashSet<String> = prog
        .fns
        .iter()
        .filter(|f| {
            let Some((params, ret)) = env.sigs.get(f.name.as_str()) else { return false };
            let first = f.params.iter().zip(params).map(|(p, t)| (p.name.clone(), t.clone())).collect();
            Check { env: &env, scopes: vec![first], ret: ret.clone(), fails: f.fails.clone() }.block(&f.body)
        })
        .map(|f| f.name.clone())
        .collect();
    // Remove any function that calls one outside the set, until nothing changes.
    loop {
        let before = set.len();
        let keep: HashSet<String> = prog
            .fns
            .iter()
            .filter(|f| set.contains(&f.name) && callees(&f.body).iter().all(|c| set.contains(c)))
            .map(|f| f.name.clone())
            .collect();
        set = keep;
        if set.len() == before {
            return set;
        }
    }
}

fn callees(body: &[Stmt]) -> Vec<String> {
    fn expr(e: &Expr, out: &mut Vec<String>) {
        // A function made into a value here is called from here, through it.
        if let Expr::Call { name, .. } | Expr::Closure { name, .. } = e {
            out.push(name.clone());
        }
        for c in e.children() {
            expr(c, out);
        }
    }
    let mut out = Vec::new();
    let mut stack: Vec<&[Stmt]> = vec![body];
    while let Some(b) = stack.pop() {
        for s in b {
            for e in s.exprs() {
                expr(e, &mut out);
            }
            stack.extend(s.blocks());
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// Code generation

/// The compiled functions of one program.
pub struct Native {
    module: Option<JITModule>,
    /// Entry wrappers by function name, as addresses (code owned by `module`).
    entries: HashMap<String, (usize, Vec<Ty>, Ty)>,
    tables: Box<Tables>,
}

struct Helpers {
    step: FuncId,
    depth: FuncId,
    arith: FuncId,
    neg: FuncId,
    no_return: FuncId,
    konst: FuncId,
    clone: FuncId,
    drop: FuncId,
    boxv: FuncId,
    unbox: FuncId,
    arith_val: FuncId,
    cmp: FuncId,
    builtin: FuncId,
    record: FuncId,
    field: FuncId,
    field_ref: FuncId,
    index_ref: FuncId,
    list: FuncId,
    index: FuncId,
    list_len: FuncId,
    list_get: FuncId,
    fail: FuncId,
    variant: FuncId,
    variant_index: FuncId,
    no_arm: FuncId,
    none: FuncId,
    some: FuncId,
    opt_has: FuncId,
    opt_get_ref: FuncId,
    opt_no_arm: FuncId,
    tuple: FuncId,
    map: FuncId,
    empty: FuncId,
    closure: FuncId,
    call_value: FuncId,
    with_function: FuncId,
}

impl Native {
    /// Compiles every eligible function. Nothing is compiled for a program with none.
    pub fn compile(prog: &Program) -> Result<Native, String> {
        let set = eligible(prog);
        if set.is_empty() {
            return Ok(Native { module: None, entries: HashMap::new(), tables: Box::new(Tables::default()) });
        }
        let isa = isa(true)?;
        let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        for (n, p) in HELPER_SYMBOLS {
            jb.symbol(*n, *p);
        }
        let mut module = JITModule::new(jb);
        let (wrappers, tables) = codegen(prog, &mut module, &set)?;
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let env = Env::new(prog);
        let mut entries = HashMap::new();
        for (name, wid) in wrappers {
            let p = module.get_finalized_function(wid);
            let (params, ret) = env.sigs[name.as_str()].clone();
            entries.insert(name, (p as usize, params, ret));
        }
        let mut tables = Box::new(tables);
        tables.funcs = entries.iter().map(|(n, (p, params, ret))| (n.clone(), (*p, params.clone(), ret.clone()))).collect();
        Ok(Native { module: Some(module), entries, tables })
    }

    /// Native code linked into an executable by `staalc build`: `fns` are the entry
    /// wrappers, in the order `compiled` names them. The program must be the one the
    /// code was made from; anything that does not match is refused.
    pub fn from_linked(prog: &Program, compiled: &[String], fns: &[usize], tables: &Json) -> Result<Native, String> {
        let set = eligible(prog);
        let mut names: Vec<&String> = set.iter().collect();
        names.sort();
        let mut given: Vec<&String> = compiled.iter().collect();
        given.sort();
        if names != given || compiled.len() != fns.len() {
            return Err("the executable's native code does not match its program".into());
        }
        let env = Env::new(prog);
        let mut entries = HashMap::new();
        for (name, p) in compiled.iter().zip(fns) {
            if *p == 0 {
                return Err(format!("the executable has no native code for '{name}'"));
            }
            let (params, ret) = env.sigs[name.as_str()].clone();
            entries.insert(name.clone(), (*p, params, ret));
        }
        let mut tables = Box::new(Tables::from_json(tables)?);
        tables.funcs = entries.iter().map(|(n, (p, params, ret))| (n.clone(), (*p, params.clone(), ret.clone()))).collect();
        Ok(Native { module: None, entries, tables })
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// The names of the compiled functions, sorted.
    pub fn compiled(&self) -> Vec<String> {
        let mut v: Vec<String> = self.entries.keys().cloned().collect();
        v.sort();
        v
    }

    /// Runs compiled function `name`. `ctx.depth` is the number of calls active,
    /// including this one. None if `name` is not compiled or the arguments do not fit
    /// its parameters, which the caller treats as "run it on the interpreter". The
    /// arena is empty again when this returns, whatever happened.
    pub fn call(&self, name: &str, ctx: &mut Ctx, args: &[Value]) -> Option<Result<Value, Failure>> {
        let (ptr, params, ret) = self.entries.get(name)?;
        if params.len() != args.len() {
            return None;
        }
        // A function value passed in must be one native code can call: its function
        // compiled, and so every function value it holds.
        fn callable(v: &Value, entries: &HashMap<String, (usize, Vec<Ty>, Ty)>) -> bool {
            match v {
                Value::Func { name, captured } => entries.contains_key(name) && captured.iter().all(|c| callable(c, entries)),
                _ => true,
            }
        }
        if !args.iter().all(|a| callable(a, &self.entries)) {
            return None;
        }
        let mut argv: Vec<i64> = Vec::with_capacity(args.len());
        for (a, t) in args.iter().zip(params) {
            match (a, rep(t)) {
                (Value::Int(n), Rep::Raw) if is(t, "Int") => argv.push(*n),
                (Value::Bool(b), Rep::Raw) if is(t, "Bool") => argv.push(*b as i64),
                (_, Rep::Raw) => return None,
                (v, Rep::Handle) => argv.push(ctx.put(v.clone())),
            }
        }
        ctx.tables = &*self.tables;
        ctx.failed = 0;
        ctx.failure = None;
        // SAFETY: `ptr` is a finalized wrapper with signature (ctx, argv) -> i64,
        // generated above; `argv` holds exactly as many i64 as the function takes; the
        // module that owns the code lives as long as `self`.
        let f = unsafe { std::mem::transmute::<usize, extern "C" fn(*mut Ctx, *const i64) -> i64>(*ptr) };
        let r = f(ctx as *mut Ctx, argv.as_ptr());
        let out = if ctx.failed != 0 {
            Err(ctx.failure.take().unwrap_or_else(|| internal("native code failed without saying why")))
        } else if is(ret, "Int") {
            Ok(Value::Int(r))
        } else if is(ret, "Bool") {
            Ok(Value::Bool(r != 0))
        } else {
            ctx.take(r)
        };
        ctx.arena.clear();
        ctx.free.clear();
        ctx.tables = std::ptr::null();
        Some(out)
    }
}

/// The helpers native code calls, by the symbol it calls them by.
const HELPER_SYMBOLS: &[(&str, *const u8)] = &[
    ("staal_rt_step_limit", rt_step_limit as *const u8),
    ("staal_rt_depth_limit", rt_depth_limit as *const u8),
    ("staal_rt_arith", rt_arith as *const u8),
    ("staal_rt_neg", rt_neg as *const u8),
    ("staal_rt_no_return", rt_no_return as *const u8),
    ("staal_rt_const", rt_const as *const u8),
    ("staal_rt_clone", rt_clone as *const u8),
    ("staal_rt_drop", rt_drop as *const u8),
    ("staal_rt_box", rt_box as *const u8),
    ("staal_rt_unbox", rt_unbox as *const u8),
    ("staal_rt_arith_val", rt_arith_val as *const u8),
    ("staal_rt_cmp", rt_cmp as *const u8),
    ("staal_rt_builtin", rt_builtin as *const u8),
    ("staal_rt_record", rt_record as *const u8),
    ("staal_rt_field", rt_field as *const u8),
    ("staal_rt_field_ref", rt_field_ref as *const u8),
    ("staal_rt_index_ref", rt_index_ref as *const u8),
    ("staal_rt_list", rt_list as *const u8),
    ("staal_rt_index", rt_index as *const u8),
    ("staal_rt_list_len", rt_list_len as *const u8),
    ("staal_rt_list_get", rt_list_get as *const u8),
    ("staal_rt_fail", rt_fail as *const u8),
    ("staal_rt_variant", rt_variant as *const u8),
    ("staal_rt_variant_index", rt_variant_index as *const u8),
    ("staal_rt_no_arm", rt_no_arm as *const u8),
    ("staal_rt_none", rt_none as *const u8),
    ("staal_rt_some", rt_some as *const u8),
    ("staal_rt_opt_has", rt_opt_has as *const u8),
    ("staal_rt_opt_get_ref", rt_opt_get_ref as *const u8),
    ("staal_rt_opt_no_arm", rt_opt_no_arm as *const u8),
    ("staal_rt_tuple", rt_tuple as *const u8),
    ("staal_rt_map", rt_map as *const u8),
    ("staal_rt_empty", rt_empty as *const u8),
    ("staal_rt_closure", rt_closure as *const u8),
    ("staal_rt_call_value", rt_call_value as *const u8),
    ("staal_rt_with_function", rt_with_function as *const u8),
];

/// The machine to generate code for: this one. `host_features`: use every feature
/// this processor has (for code run here); without, only the baseline the target
/// guarantees, so an executable runs on any processor of the same family.
fn isa(host_features: bool) -> Result<cranelift_codegen::isa::OwnedTargetIsa, String> {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    flags.set("is_pic", if host_features { "false" } else { "true" }).map_err(|e| e.to_string())?;
    flags.set("use_colocated_libcalls", "false").map_err(|e| e.to_string())?;
    let builder = cranelift_native::builder_with_options(host_features)
        .map_err(|e| format!("native code is not available on this machine: {e}"))?;
    let isa = builder.finish(settings::Flags::new(flags)).map_err(|e| e.to_string())?;
    if isa.pointer_type() != types::I64 {
        return Err("native code needs a 64-bit machine".into());
    }
    Ok(isa)
}

/// Generates every function in `set` and its entry wrapper into `module`. Returns the
/// wrappers by function name, in a fixed order, and the tables the code refers to.
fn codegen<M: Module>(prog: &Program, module: &mut M, set: &HashSet<String>) -> Result<(Vec<(String, FuncId)>, Tables), String> {
    let env = Env::new(prog);
    let mut tables = Tables::default();
    // A helper takes ctx and `extra` i64 values, and returns one if `ret`.
    let helper = |m: &mut M, name: &str, extra: usize, ret: bool| -> Result<FuncId, String> {
        let mut sig = m.make_signature();
        for _ in 0..=extra {
            sig.params.push(AbiParam::new(types::I64));
        }
        if ret {
            sig.returns.push(AbiParam::new(types::I64));
        }
        m.declare_function(name, Linkage::Import, &sig).map_err(|e| e.to_string())
    };
    let h = Helpers {
        step: helper(module, "staal_rt_step_limit", 0, false)?,
        depth: helper(module, "staal_rt_depth_limit", 0, false)?,
        arith: helper(module, "staal_rt_arith", 3, false)?,
        neg: helper(module, "staal_rt_neg", 1, false)?,
        no_return: helper(module, "staal_rt_no_return", 0, false)?,
        konst: helper(module, "staal_rt_const", 1, true)?,
        clone: helper(module, "staal_rt_clone", 1, true)?,
        drop: helper(module, "staal_rt_drop", 1, false)?,
        boxv: helper(module, "staal_rt_box", 2, true)?,
        unbox: helper(module, "staal_rt_unbox", 1, true)?,
        arith_val: helper(module, "staal_rt_arith_val", 3, true)?,
        cmp: helper(module, "staal_rt_cmp", 3, true)?,
        builtin: helper(module, "staal_rt_builtin", 3, true)?,
        record: helper(module, "staal_rt_record", 3, true)?,
        field: helper(module, "staal_rt_field", 2, true)?,
        field_ref: helper(module, "staal_rt_field_ref", 2, true)?,
        index_ref: helper(module, "staal_rt_index_ref", 2, true)?,
        list: helper(module, "staal_rt_list", 2, true)?,
        index: helper(module, "staal_rt_index", 2, true)?,
        list_len: helper(module, "staal_rt_list_len", 1, true)?,
        list_get: helper(module, "staal_rt_list_get", 2, true)?,
        fail: helper(module, "staal_rt_fail", 1, false)?,
        variant: helper(module, "staal_rt_variant", 3, true)?,
        variant_index: helper(module, "staal_rt_variant_index", 2, true)?,
        no_arm: helper(module, "staal_rt_no_arm", 1, false)?,
        none: helper(module, "staal_rt_none", 0, true)?,
        some: helper(module, "staal_rt_some", 1, true)?,
        opt_has: helper(module, "staal_rt_opt_has", 1, true)?,
        opt_get_ref: helper(module, "staal_rt_opt_get_ref", 1, true)?,
        opt_no_arm: helper(module, "staal_rt_opt_no_arm", 0, false)?,
        tuple: helper(module, "staal_rt_tuple", 2, true)?,
        map: helper(module, "staal_rt_map", 2, true)?,
        empty: helper(module, "staal_rt_empty", 1, true)?,
        closure: helper(module, "staal_rt_closure", 3, true)?,
        call_value: helper(module, "staal_rt_call_value", 3, true)?,
        with_function: helper(module, "staal_rt_with_function", 3, true)?,
    };
    // In program order, so the same program always gives the same code and tables.
    let fns: Vec<&FnDecl> = prog.fns.iter().filter(|f| set.contains(&f.name)).collect();
    let mut ids: HashMap<String, FuncId> = HashMap::new();
    let mut wrappers: Vec<(String, FuncId)> = Vec::new();
    for (i, f) in fns.iter().enumerate() {
        let mut sig = module.make_signature();
        for _ in 0..=f.params.len() {
            sig.params.push(AbiParam::new(types::I64));
        }
        sig.returns.push(AbiParam::new(types::I64));
        let id = module.declare_function(&format!("staal_f{i}"), Linkage::Local, &sig).map_err(|e| e.to_string())?;
        let mut wsig = module.make_signature();
        wsig.params.push(AbiParam::new(types::I64));
        wsig.params.push(AbiParam::new(types::I64));
        wsig.returns.push(AbiParam::new(types::I64));
        let wid = module.declare_function(&format!("staal_w{i}"), Linkage::Local, &wsig).map_err(|e| e.to_string())?;
        ids.insert(f.name.clone(), id);
        wrappers.push((f.name.clone(), wid));
    }
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    for (f, (_, wid)) in fns.iter().zip(&wrappers) {
        let id = ids[&f.name];
        ctx.func.signature = module.declarations().get_function_decl(id).signature.clone();
        ctx.func.name = UserFuncName::user(0, id.as_u32());
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let g = Gen {
                b,
                module: &mut *module,
                h: &h,
                ids: &ids,
                env: &env,
                tables: &mut tables,
                ctx: ir::Value::from_u32(0),
                scopes: Vec::new(),
                exits: Vec::new(),
                heads: Vec::new(),
            };
            g.function(f)?;
        }
        module.define_function(id, &mut ctx).map_err(|e| format!("native code for '{}': {e:?}", f.name))?;
        module.clear_context(&mut ctx);

        // The wrapper: (ctx, argv) -> the function's result, reading arguments from argv.
        ctx.func.signature = module.declarations().get_function_decl(*wid).signature.clone();
        ctx.func.name = UserFuncName::user(0, wid.as_u32());
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let (c, argv) = (b.block_params(entry)[0], b.block_params(entry)[1]);
            let mut args = vec![c];
            for i in 0..f.params.len() {
                args.push(b.ins().load(types::I64, MemFlagsData::trusted(), argv, (i * 8) as i32));
            }
            let callee = module.declare_func_in_func(id, b.func);
            let call = b.ins().call(callee, &args);
            let r = b.inst_results(call)[0];
            b.ins().return_(&[r]);
            b.seal_all_blocks();
            b.finalize(module.target_config());
        }
        module.define_function(*wid, &mut ctx).map_err(|e| format!("native entry for '{}': {e:?}", f.name))?;
        module.clear_context(&mut ctx);
    }
    Ok((wrappers, tables))
}

impl Tables {
    fn to_json(&self) -> Result<Json, String> {
        let consts: Vec<Json> = self
            .consts
            .iter()
            .map(|v| match v {
                Value::Text(t) => Ok(json!({ "text": t })),
                Value::Money(_) => Ok(json!({ "money": to_json(v, false)? })),
                _ => Err("native code: a constant that is neither Text nor Money".to_string()),
            })
            .collect::<Result<_, _>>()?;
        Ok(json!({ "consts": consts, "records": self.records, "fields": self.fields, "variants": self.variants, "enums": self.enums }))
    }

    fn from_json(j: &Json) -> Result<Tables, String> {
        let bad = || "the executable's tables are damaged".to_string();
        let mut consts = Vec::new();
        for c in j["consts"].as_array().ok_or_else(bad)? {
            consts.push(match (c.get("text"), c.get("money")) {
                (Some(Json::String(t)), None) => Value::Text(t.clone()),
                (None, Some(m)) => from_json(&named("Money"), m, &Default::default(), "constant").map_err(|e| format!("{}: {e}", bad()))?,
                _ => return Err(bad()),
            });
        }
        let records: Vec<(String, Vec<String>)> = serde_json::from_value(j["records"].clone()).map_err(|_| bad())?;
        let fields: Vec<String> = serde_json::from_value(j["fields"].clone()).map_err(|_| bad())?;
        let variants: Vec<(String, String, Vec<String>)> = serde_json::from_value(j["variants"].clone()).map_err(|_| bad())?;
        let enums: Vec<(String, Vec<String>)> = serde_json::from_value(j["enums"].clone()).map_err(|_| bad())?;
        Ok(Tables { consts, records, fields, variants, enums, funcs: HashMap::new() })
    }
}

/// An object file for `staalc build`: the program's native code, its tables, and
/// `main`, which hands everything to `staal_aot_main` in the runtime it is linked
/// with. `bundle` builds the data the executable carries (its program and tables),
/// given the tables and the compiled functions in table order.
pub fn compile_object(prog: &Program, bundle: impl FnOnce(Json, &[String]) -> Vec<u8>) -> Result<Vec<u8>, String> {
    use cranelift_module::DataDescription;
    use cranelift_object::{ObjectBuilder, ObjectModule};
    let set = eligible(prog);
    let ob = ObjectBuilder::new(isa(false)?, "staal_program", cranelift_module::default_libcall_names()).map_err(|e| e.to_string())?;
    let mut module = ObjectModule::new(ob);
    let (wrappers, tables) = codegen(prog, &mut module, &set)?;
    let names: Vec<String> = wrappers.iter().map(|(n, _)| n.clone()).collect();
    let data = bundle(tables.to_json()?, &names);

    // The bundle, and the table of entry wrappers in the same order as `names`.
    let bundle_id = module.declare_data("staal_aot_bundle", Linkage::Local, false, false).map_err(|e| e.to_string())?;
    let mut d = DataDescription::new();
    d.define(data.clone().into_boxed_slice());
    module.define_data(bundle_id, &d).map_err(|e| e.to_string())?;
    let table_id = module.declare_data("staal_aot_table", Linkage::Local, false, false).map_err(|e| e.to_string())?;
    let mut d = DataDescription::new();
    d.define(vec![0u8; 8 * wrappers.len().max(1)].into_boxed_slice());
    d.set_align(8);
    for (i, (_, wid)) in wrappers.iter().enumerate() {
        let fref = module.declare_func_in_data(*wid, &mut d);
        d.write_function_addr((i * 8) as u32, fref);
    }
    module.define_data(table_id, &d).map_err(|e| e.to_string())?;

    // int main(int argc, char **argv) { return staal_aot_main(argc, argv, bundle, len, table, n); }
    let mut aot_sig = module.make_signature();
    aot_sig.params.push(AbiParam::new(types::I32));
    for _ in 0..5 {
        aot_sig.params.push(AbiParam::new(types::I64));
    }
    aot_sig.returns.push(AbiParam::new(types::I32));
    let aot_main = module.declare_function("staal_aot_main", Linkage::Import, &aot_sig).map_err(|e| e.to_string())?;
    let mut main_sig = module.make_signature();
    main_sig.params.push(AbiParam::new(types::I32));
    main_sig.params.push(AbiParam::new(types::I64));
    main_sig.returns.push(AbiParam::new(types::I32));
    let main_id = module.declare_function("main", Linkage::Export, &main_sig).map_err(|e| e.to_string())?;
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    ctx.func.signature = main_sig;
    ctx.func.name = UserFuncName::user(0, main_id.as_u32());
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let (argc, argv) = (b.block_params(entry)[0], b.block_params(entry)[1]);
        let gb = module.declare_data_in_func(bundle_id, b.func);
        let gt = module.declare_data_in_func(table_id, b.func);
        let pb = b.ins().symbol_value(types::I64, gb);
        let pt = b.ins().symbol_value(types::I64, gt);
        let len = b.ins().iconst(types::I64, data.len() as i64);
        let n = b.ins().iconst(types::I64, wrappers.len() as i64);
        let callee = module.declare_func_in_func(aot_main, b.func);
        let call = b.ins().call(callee, &[argc, argv, pb, len, pt, n]);
        let r = b.inst_results(call)[0];
        b.ins().return_(&[r]);
        b.seal_all_blocks();
        b.finalize(module.target_config());
    }
    module.define_function(main_id, &mut ctx).map_err(|e| format!("main: {e:?}"))?;
    module.finish().emit().map_err(|e| e.to_string())
}

impl Drop for Native {
    fn drop(&mut self) {
        if let Some(m) = self.module.take() {
            // SAFETY: no pointer into the module's code outlives `self`; `entries` is
            // dropped with it and `call` cannot run concurrently with drop.
            unsafe { m.free_memory() };
        }
    }
}

struct Var {
    var: Variable,
    ty: Ty,
}

struct Gen<'b, 'm, 'p, M: Module> {
    b: FunctionBuilder<'b>,
    module: &'m mut M,
    h: &'m Helpers,
    ids: &'m HashMap<String, FuncId>,
    env: &'m Env<'p>,
    tables: &'m mut Tables,
    ctx: ir::Value,
    scopes: Vec<Vec<(String, Var)>>,
    /// For each enclosing loop: where `break` jumps, and how many scopes are open
    /// outside its body (a `break` drops the values of every scope inside).
    exits: Vec<(ir::Block, usize)>,
    /// Where `continue` jumps for each enclosing loop, and the scope depth it drops to.
    heads: Vec<(ir::Block, usize)>,
}

impl<M: Module> Gen<'_, '_, '_, M> {
    fn function(mut self, f: &FnDecl) -> Result<(), String> {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        let params: Vec<ir::Value> = self.b.block_params(entry).to_vec();
        self.ctx = params[0];
        let sig = self.env.sigs[f.name.as_str()].0.clone();
        let mut first = Vec::new();
        for ((p, v), t) in f.params.iter().zip(&params[1..]).zip(sig) {
            let var = self.b.declare_var(types::I64);
            self.b.def_var(var, *v);
            first.push((p.name.clone(), Var { var, ty: t }));
        }
        self.scopes.push(first);
        let falls = self.block(&f.body)?;
        if falls {
            // The checker guarantees a typed function returns; kept for parity.
            self.call0(self.h.no_return, &[]);
            self.fail_return();
        }
        self.b.seal_all_blocks();
        let cfg = self.module.target_config();
        self.b.finalize(cfg);
        Ok(())
    }

    /// Calls a helper with ctx and `extra`; returns its result if it has one.
    fn call0(&mut self, id: FuncId, extra: &[ir::Value]) -> Option<ir::Value> {
        let fref = self.module.declare_func_in_func(id, self.b.func);
        let mut args = vec![self.ctx];
        args.extend_from_slice(extra);
        let call = self.b.ins().call(fref, &args);
        self.b.inst_results(call).first().copied()
    }

    /// Calls a helper that can fail; returns from the function at once if it did.
    fn call_checked(&mut self, id: FuncId, extra: &[ir::Value]) -> ir::Value {
        let r = self.call0(id, extra).expect("a helper with a result");
        self.check_failed();
        r
    }

    fn check_failed(&mut self) {
        let failed = self.b.ins().load(types::I64, MemFlagsData::trusted(), self.ctx, OFF_FAILED);
        let (bad, ok) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(failed, bad, &[], ok, &[]);
        self.b.switch_to_block(bad);
        self.fail_return();
        self.b.switch_to_block(ok);
    }

    /// Returns 0 from the function: the failure is already recorded in Ctx.
    fn fail_return(&mut self) {
        let z = self.b.ins().iconst(types::I64, 0);
        self.b.ins().return_(&[z]);
    }

    /// Records a failure through `helper` and returns when `cond` is set.
    fn fail_if(&mut self, cond: ir::Value, helper: FuncId, extra: &[ir::Value]) {
        let bad = self.b.create_block();
        let ok = self.b.create_block();
        self.b.ins().brif(cond, bad, &[], ok, &[]);
        self.b.switch_to_block(bad);
        self.call0(helper, extra);
        self.fail_return();
        self.b.switch_to_block(ok);
    }

    /// One step: `steps += 1`, failing past `max_steps` (the interpreter's `tick`).
    fn tick(&mut self) {
        let flags = MemFlagsData::trusted();
        let steps = self.b.ins().load(types::I64, flags, self.ctx, OFF_STEPS);
        let steps = self.b.ins().iadd_imm_s(steps, 1);
        self.b.ins().store(flags, steps, self.ctx, OFF_STEPS);
        let max = self.b.ins().load(types::I64, flags, self.ctx, OFF_MAX_STEPS);
        let over = self.b.ins().icmp(IntCC::UnsignedGreaterThan, steps, max);
        self.fail_if(over, self.h.step, &[]);
    }

    fn lookup(&self, n: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|s| s.iter().rev().find(|(name, _)| name == n).map(|(_, v)| v))
    }

    fn ty(&self, e: &Expr) -> Result<Ty, String> {
        self.env.ty(e, &|n| self.lookup(n).map(|v| v.ty.clone())).ok_or_else(|| "native code: an expression it cannot type".to_string())
    }

    /// Drops the values held by the variables of scopes `from..`.
    fn drop_scopes_from(&mut self, from: usize) {
        let handles: Vec<Variable> =
            self.scopes[from..].iter().flatten().filter(|(_, v)| rep(&v.ty) == Rep::Handle).map(|(_, v)| v.var).collect();
        for var in handles {
            let h = self.b.use_var(var);
            self.call0(self.h.drop, &[h]);
        }
    }

    fn drop_value(&mut self, v: ir::Value, t: &Ty) {
        if rep(t) == Rep::Handle {
            self.call0(self.h.drop, &[v]);
        }
    }

    /// Compiles a block in a new scope. Returns whether control can fall out of it.
    fn block(&mut self, body: &[Stmt]) -> Result<bool, String> {
        self.scopes.push(Vec::new());
        let mut falls = true;
        for s in body {
            if !self.stmt(s)? {
                falls = false;
                break;
            }
        }
        if falls {
            let top = self.scopes.len() - 1;
            self.drop_scopes_from(top);
        }
        self.scopes.pop();
        Ok(falls)
    }

    fn declare(&mut self, name: &str, v: ir::Value, ty: Ty) {
        let var = self.b.declare_var(types::I64);
        self.b.def_var(var, v);
        self.scopes.last_mut().expect("a scope").push((name.to_string(), Var { var, ty }));
    }

    /// Compiles one statement. Returns whether control continues after it.
    fn stmt(&mut self, s: &Stmt) -> Result<bool, String> {
        self.tick();
        match s {
            Stmt::Let { name, ty: Some(d @ (Ty::List(..) | Ty::Set(..) | Ty::Map(..))), value, .. } if empty_literal(value, d) => {
                let kind = match d {
                    Ty::List(..) => 0,
                    Ty::Set(..) => 1,
                    _ => 2,
                };
                let k = self.b.ins().iconst(types::I64, kind);
                let v = self.call_checked(self.h.empty, &[k]);
                self.declare(name, v, d.clone());
                Ok(true)
            }
            Stmt::Let { name, ty, value, .. } => {
                // A typed `let` holds its declared type, which the value fits.
                let t = match ty {
                    Some(d) => d.clone(),
                    None => self.ty(value)?,
                };
                let v = self.expr(value)?;
                self.declare(name, v, t);
                Ok(true)
            }
            Stmt::Set { name, value, .. } => {
                let v = self.expr(value)?;
                let (var, t) = {
                    let x = self.lookup(name).ok_or(format!("native code: unknown variable '{name}'"))?;
                    (x.var, x.ty.clone())
                };
                let old = self.b.use_var(var);
                self.b.def_var(var, v);
                self.drop_value(old, &t);
                Ok(true)
            }
            Stmt::Expr(e) => {
                let t = self.ty(e)?;
                let v = self.expr(e)?;
                self.drop_value(v, &t);
                Ok(true)
            }
            Stmt::Return(Some(e), _) => {
                let v = self.expr(e)?;
                self.drop_scopes_from(0);
                self.b.ins().return_(&[v]);
                Ok(false)
            }
            Stmt::Fail(e, _) => {
                let v = self.expr(e)?;
                self.call0(self.h.fail, &[v]);
                self.fail_return();
                Ok(false)
            }
            Stmt::Break(_) => {
                let (exit, depth) = *self.exits.last().ok_or("native code: `break` outside a loop")?;
                self.drop_scopes_from(depth);
                self.b.ins().jump(exit, &[]);
                Ok(false)
            }
            Stmt::Continue(_) => {
                let (head, depth) = *self.heads.last().ok_or("native code: `continue` outside a loop")?;
                self.drop_scopes_from(depth);
                self.b.ins().jump(head, &[]);
                Ok(false)
            }
            Stmt::If { cond, then, els, .. } => {
                let c = self.expr(cond)?;
                let (tb, eb, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().brif(c, tb, &[], eb, &[]);
                self.b.switch_to_block(tb);
                let t_falls = self.block(then)?;
                if t_falls {
                    self.b.ins().jump(join, &[]);
                }
                self.b.switch_to_block(eb);
                let e_falls = self.block(els)?;
                if e_falls {
                    self.b.ins().jump(join, &[]);
                }
                self.b.switch_to_block(join);
                if !t_falls && !e_falls {
                    // Nothing reaches here; end the block validly.
                    self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                    return Ok(false);
                }
                Ok(true)
            }
            Stmt::Repeat { count, body, .. } => {
                let i = self.b.declare_var(types::I64);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.def_var(i, zero);
                let (head, body_b, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().jump(head, &[]);
                self.b.switch_to_block(head);
                let iv = self.b.use_var(i);
                // `count` is at most u64::MAX; compare unsigned.
                let n = self.b.ins().iconst(types::I64, *count as i64);
                let more = self.b.ins().icmp(IntCC::UnsignedLessThan, iv, n);
                self.b.ins().brif(more, body_b, &[], exit, &[]);
                self.b.switch_to_block(body_b);
                self.tick();
                let next = self.b.ins().iadd_imm_s(iv, 1);
                self.b.def_var(i, next);
                self.exits.push((exit, self.scopes.len()));
                self.heads.push((head, self.scopes.len()));
                let falls = self.block(body)?;
                self.exits.pop();
                self.heads.pop();
                if falls {
                    self.b.ins().jump(head, &[]);
                }
                self.b.switch_to_block(exit);
                Ok(true)
            }
            Stmt::Loop { body, .. } => {
                let (head, exit) = (self.b.create_block(), self.b.create_block());
                self.b.ins().jump(head, &[]);
                self.b.switch_to_block(head);
                self.tick();
                self.exits.push((exit, self.scopes.len()));
                self.heads.push((head, self.scopes.len()));
                let falls = self.block(body)?;
                self.exits.pop();
                self.heads.pop();
                if falls {
                    self.b.ins().jump(head, &[]);
                }
                self.b.switch_to_block(exit);
                Ok(true)
            }
            Stmt::For { var, list, body, .. } => {
                let Ty::List(inner, _) = self.ty(list)? else { return Err("native code: `for` over a non-list".into()) };
                // The list is evaluated once. It lives in a scope of its own around the
                // loop, so a `return` inside drops it; a `break` and the loop's end jump
                // to `exit`, which drops it.
                let lv = self.expr(list)?;
                self.scopes.push(Vec::new());
                let list_var = self.b.declare_var(types::I64);
                self.b.def_var(list_var, lv);
                let list_ty = Ty::List(inner.clone(), 0);
                self.scopes.last_mut().expect("a scope").push(("\0list".into(), Var { var: list_var, ty: list_ty }));
                let n = self.call_checked(self.h.list_len, &[lv]);
                let i = self.b.declare_var(types::I64);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.b.def_var(i, zero);
                let (head, body_b, exit) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
                self.b.ins().jump(head, &[]);
                self.b.switch_to_block(head);
                let iv = self.b.use_var(i);
                let more = self.b.ins().icmp(IntCC::SignedLessThan, iv, n);
                self.b.ins().brif(more, body_b, &[], exit, &[]);
                self.b.switch_to_block(body_b);
                self.tick();
                let next = self.b.ins().iadd_imm_s(iv, 1);
                self.b.def_var(i, next);
                let l = self.b.use_var(list_var);
                let item = self.call_checked(self.h.list_get, &[l, iv]);
                let item = self.unhandle(item, &inner);
                // The item's scope, then the body's (`block` makes that one).
                self.scopes.push(Vec::new());
                self.declare(var, item, *inner);
                let item_scope = self.scopes.len() - 1;
                self.exits.push((exit, item_scope));
                // `continue` drops the item, as the end of the body does, and goes on.
                self.heads.push((head, item_scope));
                let falls = self.block(body)?;
                self.exits.pop();
                self.heads.pop();
                if falls {
                    self.drop_scopes_from(item_scope);
                    self.b.ins().jump(head, &[]);
                }
                self.scopes.pop();
                self.b.switch_to_block(exit);
                let l = self.b.use_var(list_var);
                self.call0(self.h.drop, &[l]);
                self.scopes.pop();
                Ok(true)
            }
            Stmt::Match { value, arms, .. } => {
                let vt = self.ty(value)?;
                if let Ty::Optional(inner) = &vt {
                    return self.match_optional(value, inner, arms);
                }
                let Ty::Named(ty) = vt else { return Err("native code: `match` on a non-enum".into()) };
                let names: Vec<String> = self
                    .env
                    .enums
                    .get(ty.as_str())
                    .ok_or("native code: `match` on a non-enum")?
                    .iter()
                    .map(|v| v.name.clone())
                    .collect();
                // The value, read in place from a variable as a field read is, or else
                // evaluated into a handle this statement owns and drops before the arm.
                let (h, owned) = match self.var_handle(value) {
                    Some(v) => (v, false),
                    None => (self.expr(value)?, true),
                };
                self.tables.enums.push((ty.clone(), names.clone()));
                let eidx = self.b.ins().iconst(types::I64, (self.tables.enums.len() - 1) as i64);
                let which = self.call_checked(self.h.variant_index, &[h, eidx]);
                // The first arm whose pattern names the variant, or `_`, as the
                // interpreter picks it.
                let blocks: Vec<ir::Block> = arms.iter().map(|_| self.b.create_block()).collect();
                let mut any = false;
                for (arm, &target) in arms.iter().zip(&blocks) {
                    match &arm.pattern {
                        Pattern::Any(_) => {
                            self.b.ins().jump(target, &[]);
                            any = true;
                            break;
                        }
                        Pattern::Present { .. } | Pattern::Absent(_) => return Err("native code: an optional pattern on an enum".into()),
                        Pattern::Variant { name, .. } => {
                            let pos = names.iter().position(|n| n == name).ok_or("native code: unknown variant")?;
                            let hit = self.b.ins().icmp_imm_s(IntCC::Equal, which, pos as i64);
                            let next = self.b.create_block();
                            self.b.ins().brif(hit, target, &[], next, &[]);
                            self.b.switch_to_block(next);
                        }
                    }
                }
                if !any {
                    // The checker makes the arms cover every variant; kept for parity.
                    self.call0(self.h.no_arm, &[h]);
                    self.fail_return();
                }
                let join = self.b.create_block();
                let mut falls_any = false;
                for (arm, &target) in arms.iter().zip(&blocks) {
                    self.b.switch_to_block(target);
                    // The bindings' scope, then the body's (`block` makes that one).
                    self.scopes.push(Vec::new());
                    let bind_scope = self.scopes.len() - 1;
                    if let Pattern::Variant { name, binds, .. } = &arm.pattern {
                        let decl = self.env.variant(&ty, name).ok_or("native code: unknown variant")?;
                        for (field, to) in binds {
                            let ft = decl.iter().find(|(f, _)| f == field).ok_or("native code: unknown field")?.1.clone();
                            self.tables.fields.push(field.clone());
                            let idx = self.b.ins().iconst(types::I64, (self.tables.fields.len() - 1) as i64);
                            let fh = self.call_checked(self.h.field_ref, &[h, idx]);
                            let fv = self.unhandle(fh, &ft);
                            self.declare(to, fv, ft);
                        }
                    }
                    if owned {
                        self.call0(self.h.drop, &[h]);
                    }
                    let falls = self.block(&arm.body)?;
                    if falls {
                        self.drop_scopes_from(bind_scope);
                        self.b.ins().jump(join, &[]);
                        falls_any = true;
                    }
                    self.scopes.pop();
                }
                self.b.switch_to_block(join);
                if !falls_any {
                    // Nothing reaches here; end the block validly.
                    self.b.ins().trap(ir::TrapCode::unwrap_user(1));
                    return Ok(false);
                }
                Ok(true)
            }
            Stmt::Return(None, _) => Err("native code: statement not supported".into()),
        }
    }

    /// `match` on an optional value: the first arm that accepts it, as the interpreter
    /// picks it. Like a `match` on an enum, the value is read in place from a variable,
    /// or evaluated into a handle dropped before the arm runs.
    fn match_optional(&mut self, value: &Expr, inner: &Ty, arms: &[crate::ast::Arm]) -> Result<bool, String> {
        let (h, owned) = match self.var_handle(value) {
            Some(v) => (v, false),
            None => (self.expr(value)?, true),
        };
        let has = self.call_checked(self.h.opt_has, &[h]);
        let blocks: Vec<ir::Block> = arms.iter().map(|_| self.b.create_block()).collect();
        let mut any = false;
        for (arm, &target) in arms.iter().zip(&blocks) {
            match &arm.pattern {
                Pattern::Any(_) => {
                    self.b.ins().jump(target, &[]);
                    any = true;
                    break;
                }
                Pattern::Present { .. } => {
                    let next = self.b.create_block();
                    self.b.ins().brif(has, target, &[], next, &[]);
                    self.b.switch_to_block(next);
                }
                Pattern::Absent(_) => {
                    let next = self.b.create_block();
                    self.b.ins().brif(has, next, &[], target, &[]);
                    self.b.switch_to_block(next);
                }
                Pattern::Variant { .. } => return Err("native code: a variant pattern on an optional value".into()),
            }
        }
        if !any {
            // The checker makes the arms cover both cases; kept for parity.
            self.call0(self.h.opt_no_arm, &[]);
            self.fail_return();
        }
        let join = self.b.create_block();
        let mut falls_any = false;
        for (arm, &target) in arms.iter().zip(&blocks) {
            self.b.switch_to_block(target);
            self.scopes.push(Vec::new());
            let bind_scope = self.scopes.len() - 1;
            if let Pattern::Present { bind, .. } = &arm.pattern {
                let ih = self.call_checked(self.h.opt_get_ref, &[h]);
                let iv = self.unhandle(ih, inner);
                self.declare(bind, iv, inner.clone());
            }
            if owned {
                self.call0(self.h.drop, &[h]);
            }
            let falls = self.block(&arm.body)?;
            if falls {
                self.drop_scopes_from(bind_scope);
                self.b.ins().jump(join, &[]);
                falls_any = true;
            }
            self.scopes.pop();
        }
        self.b.switch_to_block(join);
        if !falls_any {
            self.b.ins().trap(ir::TrapCode::unwrap_user(1));
            return Ok(false);
        }
        Ok(true)
    }

    /// The handle a variable holds, when `e` is a variable holding one. Reading through
    /// it is the same as reading a copy, as values never change in place.
    fn var_handle(&mut self, e: &Expr) -> Option<ir::Value> {
        let Expr::Var(n, _) = e else { return None };
        let v = self.lookup(n)?;
        if rep(&v.ty) != Rep::Handle {
            return None;
        }
        let var = v.var;
        Some(self.b.use_var(var))
    }

    /// A handle for a value of type `t` (boxing Int and Bool).
    fn as_handle(&mut self, v: ir::Value, t: &Ty) -> ir::Value {
        match rep(t) {
            Rep::Handle => v,
            Rep::Raw => {
                let kind = self.b.ins().iconst(types::I64, if is(t, "Int") { 0 } else { 1 });
                self.call0(self.h.boxv, &[kind, v]).expect("a result")
            }
        }
    }

    /// A handle turned into the representation of type `t`.
    fn unhandle(&mut self, h: ir::Value, t: &Ty) -> ir::Value {
        match rep(t) {
            Rep::Handle => h,
            Rep::Raw => self.call_checked(self.h.unbox, &[h]),
        }
    }

    /// Stores `vals` in a stack array; returns its address.
    fn argv(&mut self, vals: &[ir::Value]) -> ir::Value {
        let size = (vals.len().max(1) * 8) as u32;
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 3));
        let addr = self.b.ins().stack_addr(types::I64, slot, 0);
        for (i, v) in vals.iter().enumerate() {
            self.b.ins().store(MemFlagsData::trusted(), *v, addr, (i * 8) as i32);
        }
        addr
    }

    fn konst(&mut self, v: Value) -> ir::Value {
        self.tables.consts.push(v);
        let idx = self.b.ins().iconst(types::I64, (self.tables.consts.len() - 1) as i64);
        self.call_checked(self.h.konst, &[idx])
    }

    fn expr(&mut self, e: &Expr) -> Result<ir::Value, String> {
        match e {
            Expr::Int(n, _) => {
                let n = i64::try_from(*n).map_err(|_| "native code: literal out of range".to_string())?;
                Ok(self.b.ins().iconst(types::I64, n))
            }
            Expr::Bool(v, _) => Ok(self.b.ins().iconst(types::I64, *v as i64)),
            Expr::Str(s, _) => Ok(self.konst(Value::Text(s.clone()))),
            Expr::Money(m, _) => Ok(self.konst(Value::Money(*m))),
            Expr::Var(n, _) => {
                let (var, t) = {
                    let x = self.lookup(n).ok_or(format!("native code: unknown variable '{n}'"))?;
                    (x.var, x.ty.clone())
                };
                let v = self.b.use_var(var);
                // Reading a variable copies its value, as the interpreter does.
                Ok(match rep(&t) {
                    Rep::Raw => v,
                    Rep::Handle => self.call_checked(self.h.clone, &[v]),
                })
            }
            Expr::Arith { op, left, right, .. } => {
                let (lt, rt) = (self.ty(left)?, self.ty(right)?);
                let l = self.expr(left)?;
                let r = self.expr(right)?;
                let code = self.b.ins().iconst(types::I64, arith_code(*op));
                if is(&lt, "Int") && is(&rt, "Int") {
                    return Ok(match op {
                        ArithOp::Add | ArithOp::Sub | ArithOp::Mul => {
                            let (v, over) = match op {
                                ArithOp::Add => self.b.ins().sadd_overflow(l, r),
                                ArithOp::Sub => self.b.ins().ssub_overflow(l, r),
                                _ => self.b.ins().smul_overflow(l, r),
                            };
                            self.fail_if(over, self.h.arith, &[code, l, r]);
                            v
                        }
                        ArithOp::Div => {
                            let zero = self.b.ins().icmp_imm_s(IntCC::Equal, r, 0);
                            let min = self.b.ins().icmp_imm_s(IntCC::Equal, l, i64::MIN);
                            let minus1 = self.b.ins().icmp_imm_s(IntCC::Equal, r, -1);
                            let both = self.b.ins().band(min, minus1);
                            let bad = self.b.ins().bor(zero, both);
                            self.fail_if(bad, self.h.arith, &[code, l, r]);
                            self.b.ins().sdiv(l, r)
                        }
                    });
                }
                let lh = self.as_handle(l, &lt);
                let rh = self.as_handle(r, &rt);
                Ok(self.call_checked(self.h.arith_val, &[code, lh, rh]))
            }
            Expr::Neg(inner, _) => {
                let v = self.expr(inner)?;
                let min = self.b.ins().icmp_imm_s(IntCC::Equal, v, i64::MIN);
                self.fail_if(min, self.h.neg, &[v]);
                Ok(self.b.ins().ineg(v))
            }
            Expr::Not(inner, _) => {
                let v = self.expr(inner)?;
                Ok(self.b.ins().bxor_imm_s(v, 1))
            }
            Expr::Logic { op, left, right, .. } => {
                // The right side runs only if it decides the answer.
                let l = self.expr(left)?;
                let result = self.b.declare_var(types::I64);
                self.b.def_var(result, l);
                let (rhs, join) = (self.b.create_block(), self.b.create_block());
                match op {
                    LogicOp::And => self.b.ins().brif(l, rhs, &[], join, &[]),
                    LogicOp::Or => self.b.ins().brif(l, join, &[], rhs, &[]),
                };
                self.b.switch_to_block(rhs);
                let r = self.expr(right)?;
                self.b.def_var(result, r);
                self.b.ins().jump(join, &[]);
                self.b.switch_to_block(join);
                Ok(self.b.use_var(result))
            }
            Expr::Cmp { op, left, right, .. } => {
                let (lt, rt) = (self.ty(left)?, self.ty(right)?);
                let l = self.expr(left)?;
                let r = self.expr(right)?;
                if rep(&lt) == Rep::Raw && rep(&rt) == Rep::Raw {
                    let cc = match op {
                        CmpOp::Eq => IntCC::Equal,
                        CmpOp::Ne => IntCC::NotEqual,
                        CmpOp::Lt => IntCC::SignedLessThan,
                        CmpOp::Le => IntCC::SignedLessThanOrEqual,
                        CmpOp::Gt => IntCC::SignedGreaterThan,
                        CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                    };
                    let c = self.b.ins().icmp(cc, l, r);
                    return Ok(self.b.ins().uextend(types::I64, c));
                }
                let code = CMPS.iter().position(|c| c == op).expect("every comparison") as i64;
                let code = self.b.ins().iconst(types::I64, code);
                let lh = self.as_handle(l, &lt);
                let rh = self.as_handle(r, &rt);
                Ok(self.call_checked(self.h.cmp, &[code, lh, rh]))
            }
            Expr::Builtin { f, args, .. } => {
                let ret = self.ty(e)?;
                let mut hs = Vec::with_capacity(args.len());
                for a in args {
                    let t = self.ty(a)?;
                    let v = self.expr(a)?;
                    hs.push(self.as_handle(v, &t));
                }
                let argv = self.argv(&hs);
                let code = BUILTINS.iter().position(|b| b == f).expect("every built-in") as i64;
                let code = self.b.ins().iconst(types::I64, code);
                let n = self.b.ins().iconst(types::I64, hs.len() as i64);
                let helper = if f.calls_function() { self.h.with_function } else { self.h.builtin };
                let h = self.call_checked(helper, &[code, argv, n]);
                Ok(self.unhandle(h, &ret))
            }
            Expr::Record { ty, fields, .. } => {
                // Evaluated in the order written, stored in declaration order.
                let decl: Vec<(String, Ty)> = self.env.records.get(ty.as_str()).ok_or("native code: unknown record")?.to_vec();
                let mut given: Vec<(String, ir::Value)> = Vec::with_capacity(fields.len());
                for (name, fe) in fields {
                    let t = self.ty(fe)?;
                    let v = self.expr(fe)?;
                    let h = self.as_handle(v, &t);
                    given.push((name.clone(), h));
                }
                let mut ordered = Vec::with_capacity(decl.len());
                for (fname, _) in &decl {
                    let i = given.iter().position(|(n, _)| n == fname).ok_or("native code: a field is missing")?;
                    ordered.push(given[i].1);
                }
                self.tables.records.push((ty.clone(), decl.iter().map(|(n, _)| n.clone()).collect()));
                let idx = self.b.ins().iconst(types::I64, (self.tables.records.len() - 1) as i64);
                let argv = self.argv(&ordered);
                let n = self.b.ins().iconst(types::I64, ordered.len() as i64);
                Ok(self.call_checked(self.h.record, &[idx, argv, n]))
            }
            Expr::Variant { ty, variant, fields, .. } => {
                // Evaluated in the order written, stored in declaration order.
                let decl: Vec<(String, Ty)> = self.env.variant(ty, variant).ok_or("native code: unknown variant")?.to_vec();
                let mut given: Vec<(String, ir::Value)> = Vec::with_capacity(fields.len());
                for (name, fe) in fields {
                    let t = self.ty(fe)?;
                    let v = self.expr(fe)?;
                    let h = self.as_handle(v, &t);
                    given.push((name.clone(), h));
                }
                let mut ordered = Vec::with_capacity(decl.len());
                for (fname, _) in &decl {
                    let i = given.iter().position(|(n, _)| n == fname).ok_or("native code: a field is missing")?;
                    ordered.push(given[i].1);
                }
                self.tables.variants.push((ty.clone(), variant.clone(), decl.iter().map(|(n, _)| n.clone()).collect()));
                let idx = self.b.ins().iconst(types::I64, (self.tables.variants.len() - 1) as i64);
                let argv = self.argv(&ordered);
                let n = self.b.ins().iconst(types::I64, ordered.len() as i64);
                Ok(self.call_checked(self.h.variant, &[idx, argv, n]))
            }
            Expr::NoneLit(_) => Ok(self.call_checked(self.h.none, &[])),
            Expr::Tuple(items, _) => {
                let mut hs = Vec::with_capacity(items.len());
                for i in items {
                    let t = self.ty(i)?;
                    let v = self.expr(i)?;
                    hs.push(self.as_handle(v, &t));
                }
                let argv = self.argv(&hs);
                let n = self.b.ins().iconst(types::I64, hs.len() as i64);
                Ok(self.call_checked(self.h.tuple, &[argv, n]))
            }
            // Keys and values alternate, evaluated in the order written.
            Expr::MapLit(entries, _) => {
                let mut hs = Vec::with_capacity(entries.len() * 2);
                for (k, v) in entries {
                    for e in [k, v] {
                        let t = self.ty(e)?;
                        let x = self.expr(e)?;
                        hs.push(self.as_handle(x, &t));
                    }
                }
                let argv = self.argv(&hs);
                let n = self.b.ins().iconst(types::I64, hs.len() as i64);
                Ok(self.call_checked(self.h.map, &[argv, n]))
            }
            Expr::SomeOf(inner, _) => {
                let t = self.ty(inner)?;
                let v = self.expr(inner)?;
                let h = self.as_handle(v, &t);
                Ok(self.call_checked(self.h.some, &[h]))
            }
            Expr::List(items, _) => {
                let mut hs = Vec::with_capacity(items.len());
                for i in items {
                    let t = self.ty(i)?;
                    let v = self.expr(i)?;
                    hs.push(self.as_handle(v, &t));
                }
                let argv = self.argv(&hs);
                let n = self.b.ins().iconst(types::I64, hs.len() as i64);
                Ok(self.call_checked(self.h.list, &[argv, n]))
            }
            Expr::Field { base, field, .. } => {
                let t = self.ty(e)?;
                self.tables.fields.push(field.clone());
                let idx = (self.tables.fields.len() - 1) as i64;
                // From a variable: read the field in place, copying only the field.
                let h = match self.var_handle(base) {
                    Some(v) => {
                        let idx = self.b.ins().iconst(types::I64, idx);
                        self.call_checked(self.h.field_ref, &[v, idx])
                    }
                    None => {
                        let b = self.expr(base)?;
                        let idx = self.b.ins().iconst(types::I64, idx);
                        self.call_checked(self.h.field, &[b, idx])
                    }
                };
                Ok(self.unhandle(h, &t))
            }
            Expr::Index { base, index, .. } => {
                let t = self.ty(e)?;
                let h = match self.var_handle(base) {
                    Some(v) => {
                        let i = self.expr(index)?;
                        self.call_checked(self.h.index_ref, &[v, i])
                    }
                    None => {
                        let l = self.expr(base)?;
                        let i = self.expr(index)?;
                        self.call_checked(self.h.index, &[l, i])
                    }
                };
                Ok(self.unhandle(h, &t))
            }
            Expr::Call { name, args, slots, .. } => {
                // Evaluated in the order written, passed in parameter order.
                let mut written = Vec::with_capacity(args.len());
                for a in args {
                    written.push(self.expr(a)?);
                }
                let mut vals = vec![self.ctx];
                if slots.is_empty() {
                    vals.extend(written);
                } else {
                    let mut ordered: Vec<Option<ir::Value>> = vec![None; written.len()];
                    for (v, &s) in written.into_iter().zip(slots) {
                        *ordered.get_mut(s).ok_or("native code: an argument without a parameter")? = Some(v);
                    }
                    for v in ordered {
                        vals.push(v.ok_or("native code: a missing argument")?);
                    }
                }
                let id = *self.ids.get(name).ok_or(format!("native code: '{name}' is not compiled"))?;
                // The interpreter's depth check, then depth + 1 around the call.
                let flags = MemFlagsData::trusted();
                let depth = self.b.ins().load(types::I64, flags, self.ctx, OFF_DEPTH);
                let max = self.b.ins().load(types::I64, flags, self.ctx, OFF_MAX_DEPTH);
                let deep = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, depth, max);
                self.fail_if(deep, self.h.depth, &[]);
                let up = self.b.ins().iadd_imm_s(depth, 1);
                self.b.ins().store(flags, up, self.ctx, OFF_DEPTH);
                let callee = self.module.declare_func_in_func(id, self.b.func);
                let call = self.b.ins().call(callee, &vals);
                let r = self.b.inst_results(call)[0];
                self.b.ins().store(flags, depth, self.ctx, OFF_DEPTH);
                self.check_failed();
                Ok(r)
            }
            Expr::Closure { name, captures, .. } => {
                let mut held = Vec::with_capacity(captures.len());
                for c in captures {
                    let t = self.ty(c)?;
                    let v = self.expr(c)?;
                    held.push(self.as_handle(v, &t));
                }
                let argv = self.argv(&held);
                self.tables.consts.push(Value::Text(name.clone()));
                let idx = self.b.ins().iconst(types::I64, (self.tables.consts.len() - 1) as i64);
                let n = self.b.ins().iconst(types::I64, held.len() as i64);
                Ok(self.call_checked(self.h.closure, &[idx, argv, n]))
            }
            // The callee first, then the arguments in the order written, as the
            // interpreter does; then the call, with the same depth check as any call.
            Expr::CallValue { callee, args, .. } => {
                let f = self.expr(callee)?;
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a)?);
                }
                let argv = self.argv(&vals);
                let n = self.b.ins().iconst(types::I64, vals.len() as i64);
                let flags = MemFlagsData::trusted();
                let depth = self.b.ins().load(types::I64, flags, self.ctx, OFF_DEPTH);
                let max = self.b.ins().load(types::I64, flags, self.ctx, OFF_MAX_DEPTH);
                let deep = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, depth, max);
                self.fail_if(deep, self.h.depth, &[]);
                let up = self.b.ins().iadd_imm_s(depth, 1);
                self.b.ins().store(flags, up, self.ctx, OFF_DEPTH);
                let r = self.call0(self.h.call_value, &[f, argv, n]).expect("a result");
                self.b.ins().store(flags, depth, self.ctx, OFF_DEPTH);
                self.check_failed();
                Ok(r)
            }
            _ => Err("native code: expression not supported".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_layout_matches_the_offsets_generated_code_uses() {
        let c = Ctx::new(0, 0, 0, 0);
        let base = &c as *const Ctx as usize;
        assert_eq!(&c.steps as *const u64 as usize - base, OFF_STEPS as usize);
        assert_eq!(&c.max_steps as *const u64 as usize - base, OFF_MAX_STEPS as usize);
        assert_eq!(&c.depth as *const u64 as usize - base, OFF_DEPTH as usize);
        assert_eq!(&c.max_depth as *const u64 as usize - base, OFF_MAX_DEPTH as usize);
        assert_eq!(&c.failed as *const u64 as usize - base, OFF_FAILED as usize);
    }

    #[test]
    fn handles_are_checked() {
        let mut c = Ctx::new(0, 0, 0, 0);
        let h = c.put(Value::Int(1));
        assert!(c.get(h).is_ok());
        assert!(c.take(h).is_ok());
        assert!(c.take(h).is_err(), "a handle is used once");
        for bad in [0, -1, 99, i64::MIN] {
            assert!(c.get(bad).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod memory {
    use super::*;

    /// A loop that rebuilds a text and a record 10,000 times holds a handful of values
    /// at a time, as the interpreter would: reassignment and scope exit drop them.
    #[test]
    fn values_are_dropped_as_the_interpreter_drops_them() {
        let src = r#"type P { name: Text, n: Int }

fn grow(k: Int) -> Int {
    let s = ""
    let p = P { name: "a", n: 0 }
    let items = [1, 2, 3]
    repeat 10000 {
        set s = s + "x"
        set p = P { name: p.name + "b", n: p.n + 1 }
        for i in items {
            let t = s + json(i)
            // Taken on most iterations: a `break` must drop what it leaves behind.
            if len(t) > 50 {
                break
            }
        }
        if p.n == k {
            return len(s) + p.n
        }
    }
    return len(s) + len(p.name)
}
"#;
        let prog = crate::check_program(src).unwrap();
        let native = Native::compile(&prog).unwrap();
        assert_eq!(native.compiled(), ["grow"]);
        for (k, want) in [(-1i64, 10000 + 10001), (500, 500 + 500)] {
            let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
            let r = native.call("grow", &mut ctx, &[Value::Int(k)]).unwrap();
            assert_eq!(r.unwrap(), Value::Int(want));
            assert!(ctx.peak <= 12, "at most {} values were alive at once", ctx.peak);
            assert_eq!(ctx.arena.len() - ctx.free.len(), 0);
        }
    }

    #[test]
    fn continue_drops_what_the_rest_of_the_iteration_would_have() {
        let src = r#"type P { name: Text, n: Int }

fn skip(k: Int) -> Int {
    let kept = 0
    let items = ["a", "bb", "ccc"]
    repeat 10000 {
        let p = P { name: "x", n: kept }
        for w in items {
            let t = w + p.name
            // Taken on two of every three items: `continue` must drop `t` and `w`.
            if len(t) < 4 {
                continue
            }
            set kept = kept + 1
        }
        if kept == k {
            continue
        }
    }
    return kept
}

fn first_with(words: List<Text, 10>, c: Text) -> Text {
    let i = 0
    loop {
        if i >= len(words) {
            return "none"
        }
        let w = words[i]
        set i = i + 1
        if not contains(w, c) {
            continue
        }
        return w
    }
}
"#;
        let prog = crate::check_program(src).unwrap();
        let native = Native::compile(&prog).unwrap();
        assert_eq!(native.compiled(), ["first_with", "skip"]);
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        assert_eq!(native.call("skip", &mut ctx, &[Value::Int(7)]).unwrap().unwrap(), Value::Int(10000));
        assert!(ctx.peak <= 12, "at most {} values were alive at once", ctx.peak);
        assert_eq!(ctx.arena.len() - ctx.free.len(), 0);
        let words = Value::List(["cat", "dog", "bird"].iter().map(|w| Value::Text(w.to_string())).collect());
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        let r = native.call("first_with", &mut ctx, &[words, Value::Text("i".into())]).unwrap().unwrap();
        assert_eq!(r, Value::Text("bird".into()));
        assert_eq!(ctx.arena.len() - ctx.free.len(), 0);
    }

    /// Enums are built and matched in native code, and a `match` holds no more values
    /// than its bindings: the matched value is read in place or dropped before the arm.
    #[test]
    fn enums_are_compiled_and_matching_drops_what_it_binds() {
        let src = r#"enum Shape {
    Dot,
    Square { side: Int, name: Text },
    Line { length: Int },
}

fn make(i: Int) -> Shape {
    if i - i / 3 * 3 == 0 {
        return Shape.Dot
    }
    if i - i / 3 * 3 == 1 {
        return Shape.Square { name: "s", side: i }
    }
    return Shape.Line { length: i }
}

fn area(s: Shape) -> Int {
    match s {
        Square { side, .. } => {
            return side * side
        }
        Line { .. } => {
            return 0
        }
        Dot => {
            return 1
        }
    }
}

fn total(n: Int) -> Int {
    let sum = 0
    let i = 0
    loop {
        if i == n {
            return sum
        }
        set i = i + 1
        match make(i) {
            Dot => {
                continue
            }
            Square { side: x, name } => {
                if x > 1000 {
                    break
                }
                set sum = sum + x + len(name)
            }
            _ => {}
        }
        set sum = sum + area(make(i))
    }
    return 0 - sum
}
"#;
        let prog = crate::check_program(src).unwrap();
        let native = Native::compile(&prog).unwrap();
        assert_eq!(native.compiled(), ["area", "make", "total"]);
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        let sq = native.call("make", &mut ctx, &[Value::Int(4)]).unwrap().unwrap();
        let want = Value::Enum {
            ty: "Shape".into(),
            variant: "Square".into(),
            fields: vec![("side".into(), Value::Int(4)), ("name".into(), Value::Text("s".into()))],
        };
        assert_eq!(sq, want);
        assert_eq!(native.call("area", &mut ctx, &[sq]).unwrap().unwrap(), Value::Int(16));
        // i = 1, 4, 7, ... are squares: each adds i + 1 and then i * i; lines add 0.
        let squares: Vec<i64> = (1..=3000).filter(|i| i % 3 == 1).collect();
        let before: i64 = squares.iter().filter(|i| **i <= 1000).map(|i| i + 1 + i * i).sum();
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        let r = native.call("total", &mut ctx, &[Value::Int(3000)]).unwrap().unwrap();
        assert_eq!(r, Value::Int(-before));
        assert!(ctx.peak <= 8, "at most {} values were alive at once", ctx.peak);
        assert_eq!(ctx.arena.len() - ctx.free.len(), 0);
    }

    /// Optional values are built and matched in native code, and a `match` on one holds
    /// no more than what it binds.
    #[test]
    fn optional_values_are_compiled_and_matching_drops_what_it_binds() {
        let src = r#"type Box { label: Text, n: Int }

fn maybe(i: Int) -> Optional<Box> {
    if i - i / 2 * 2 == 0 {
        return none
    }
    return some(Box { label: "b", n: i })
}

fn total(n: Int) -> Int {
    let total = 0
    let last: Optional<Int> = none
    let i = 0
    loop {
        if i == n {
            break
        }
        set i = i + 1
        match maybe(i) {
            none => {
                continue
            }
            some(b) => {
                set total = total + b.n + len(b.label)
                set last = some(b.n)
            }
        }
    }
    match last {
        some(l) => {
            return total * 10 + l - l
        }
        _ => {
            return 0 - 1
        }
    }
}
"#;
        let prog = crate::check_program(src).unwrap();
        let native = Native::compile(&prog).unwrap();
        assert_eq!(native.compiled(), ["maybe", "total"]);
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        assert_eq!(native.call("maybe", &mut ctx, &[Value::Int(2)]).unwrap().unwrap(), Value::Opt(None));
        let odd: i64 = (1..=3000).filter(|i| i % 2 == 1).map(|i| i + 1).sum();
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        assert_eq!(native.call("total", &mut ctx, &[Value::Int(3000)]).unwrap().unwrap(), Value::Int(odd * 10));
        assert!(ctx.peak <= 8, "at most {} values were alive at once", ctx.peak);
        let mut ctx = Ctx::new(0, 10_000_000, 1, 256);
        assert_eq!(native.call("total", &mut ctx, &[Value::Int(0)]).unwrap().unwrap(), Value::Int(-1));
    }
}
