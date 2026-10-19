// Native code for pure functions (roadmap phase 5), through Cranelift.
//
// A function is compiled when everything it does can be done without the runtime:
// its parameters and result are Int or Bool, its body uses only `let`, `set`, `if`,
// `repeat`, `loop`, `break`, `return`, arithmetic, comparisons, `and`/`or`/`not`, and
// calls to other compiled functions. Every other function runs on the interpreter, and
// a compiled function is only ever entered from it. Model calls, actions, HTTP, files
// and the journal therefore never run in native code: they stay in the Rust runtime.
//
// The interpreter is the reference. Native code reproduces it exactly:
//   - steps: one per statement and one per loop iteration, against the same limit,
//     on the same counter (passed in and out through `Ctx`);
//   - depth: checked before each call, with the same limit;
//   - Int arithmetic: checked; an overflow, a division by zero or MIN / -1 fails;
//   - evaluation order: left to right, `and`/`or` short-circuit.
// Every failure is built by the interpreter's own functions (`arith`, `neg`,
// `step_limit`, `depth_limit`), so the message is the same byte for byte.
//
// Calling convention: each function takes `*mut Ctx` and its arguments as i64 (Bool is
// 0 or 1) and returns i64. On failure it records the failure in `Ctx`, sets `failed`,
// and returns 0; every caller checks `failed` after each call and returns at once.

use crate::ast::{ArithOp, CmpOp, Expr, FnDecl, LogicOp, Program, Stmt, Ty};
use crate::interp::{arith_failure, depth_limit, neg, step_limit, Failure, FailKind};
use cranelift_codegen::ir::{self, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlagsData, UserFuncName};
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
}

const OFF_STEPS: i32 = 0;
const OFF_MAX_STEPS: i32 = 8;
const OFF_DEPTH: i32 = 16;
const OFF_MAX_DEPTH: i32 = 24;
const OFF_FAILED: i32 = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Int,
    Bool,
}

fn kind_of(t: &Ty) -> Option<K> {
    match t {
        Ty::Named(n) if n == "Int" => Some(K::Int),
        Ty::Named(n) if n == "Bool" => Some(K::Bool),
        _ => None,
    }
}

fn arith_code(op: ArithOp) -> i64 {
    match op {
        ArithOp::Add => 0,
        ArithOp::Sub => 1,
        ArithOp::Mul => 2,
        ArithOp::Div => 3,
    }
}

fn arith_op(code: i64) -> Option<ArithOp> {
    match code {
        0 => Some(ArithOp::Add),
        1 => Some(ArithOp::Sub),
        2 => Some(ArithOp::Mul),
        3 => Some(ArithOp::Div),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------
// Runtime helpers called from native code on its failure paths. They never panic:
// a panic cannot cross into generated code.

fn record(ctx: *mut Ctx, f: Failure) {
    // SAFETY: native code passes the Ctx it was given, which `Native::call` keeps alive
    // and exclusively borrowed for the whole call.
    let ctx = unsafe { &mut *ctx };
    ctx.failure = Some(f);
    ctx.failed = 1;
}

extern "C" fn rt_step_limit(ctx: *mut Ctx) {
    // SAFETY: as in `record`.
    let max = unsafe { (*ctx).max_steps };
    record(ctx, step_limit(max));
}

extern "C" fn rt_depth_limit(ctx: *mut Ctx) {
    // SAFETY: as in `record`.
    let max = unsafe { (*ctx).max_depth };
    record(ctx, depth_limit(max));
}

extern "C" fn rt_arith(ctx: *mut Ctx, op: i64, a: i64, b: i64) {
    let f = match arith_op(op) {
        Some(op) => arith_failure(op, a, b),
        None => Failure { kind: FailKind::Internal, message: format!("native code: unknown operator {op}") },
    };
    record(ctx, f);
}

extern "C" fn rt_neg(ctx: *mut Ctx, a: i64) {
    let f = match neg(a) {
        Err(f) => f,
        Ok(_) => Failure { kind: FailKind::Internal, message: format!("native code: -({a}) reported as an overflow") },
    };
    record(ctx, f);
}

extern "C" fn rt_no_return(ctx: *mut Ctx) {
    record(ctx, Failure { kind: FailKind::Internal, message: "a native function finished without returning a value".into() });
}

// ---------------------------------------------------------------------------------
// Which functions can be compiled

/// The functions that can run as native code: those whose own code qualifies and
/// whose every callee does too.
pub fn eligible(prog: &Program) -> HashSet<String> {
    let sigs: HashMap<&str, (Vec<K>, K)> = prog
        .fns
        .iter()
        .filter_map(|f| {
            let params: Option<Vec<K>> = f.params.iter().map(|p| kind_of(&p.ty)).collect();
            let ret = f.ret.as_ref().and_then(kind_of);
            Some((f.name.as_str(), (params?, ret?)))
        })
        .collect();
    let mut set: HashSet<String> = prog
        .fns
        .iter()
        .filter(|f| !f.name.contains(' ') && sigs.contains_key(f.name.as_str()) && Check::new(&sigs, f).body_ok(f))
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
        if let Expr::Call { name, .. } = e {
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

/// Checks a function's code, tracking the kind of every variable in scope.
struct Check<'a> {
    sigs: &'a HashMap<&'a str, (Vec<K>, K)>,
    scopes: Vec<HashMap<String, K>>,
    ret: K,
}

impl<'a> Check<'a> {
    fn new(sigs: &'a HashMap<&'a str, (Vec<K>, K)>, f: &FnDecl) -> Check<'a> {
        let (params, ret) = &sigs[f.name.as_str()];
        let first = f.params.iter().zip(params).map(|(p, k)| (p.name.clone(), *k)).collect();
        Check { sigs, scopes: vec![first], ret: *ret }
    }

    fn body_ok(&mut self, f: &FnDecl) -> bool {
        self.block(&f.body)
    }

    fn lookup(&self, n: &str) -> Option<K> {
        self.scopes.iter().rev().find_map(|s| s.get(n).copied())
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
                let Some(k) = self.expr(value) else { return false };
                if let Some(t) = ty
                    && kind_of(t) != Some(k)
                {
                    return false;
                }
                self.scopes.last_mut().expect("a scope").insert(name.clone(), k);
                true
            }
            Stmt::Set { name, value, .. } => self.lookup(name).is_some() && self.expr(value) == self.lookup(name),
            Stmt::Expr(e) => self.expr(e).is_some(),
            Stmt::Return(Some(e), _) => self.expr(e) == Some(self.ret),
            Stmt::If { cond, then, els, .. } => self.expr(cond) == Some(K::Bool) && self.block(then) && self.block(els),
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => self.block(body),
            Stmt::Break(_) => true,
            Stmt::Return(None, _) | Stmt::Fail(..) | Stmt::For { .. } => false,
        }
    }

    fn expr(&mut self, e: &Expr) -> Option<K> {
        match e {
            Expr::Var(n, _) => self.lookup(n),
            Expr::Int(n, _) => i64::try_from(*n).ok().map(|_| K::Int),
            Expr::Bool(..) => Some(K::Bool),
            Expr::Arith { left, right, .. } => (self.expr(left)? == K::Int && self.expr(right)? == K::Int).then_some(K::Int),
            Expr::Neg(inner, _) => (self.expr(inner)? == K::Int).then_some(K::Int),
            Expr::Not(inner, _) => (self.expr(inner)? == K::Bool).then_some(K::Bool),
            Expr::Logic { left, right, .. } => (self.expr(left)? == K::Bool && self.expr(right)? == K::Bool).then_some(K::Bool),
            Expr::Cmp { op, left, right, .. } => {
                let (l, r) = (self.expr(left)?, self.expr(right)?);
                match (l, r) {
                    (K::Int, K::Int) => Some(K::Bool),
                    (K::Bool, K::Bool) if matches!(op, CmpOp::Eq | CmpOp::Ne) => Some(K::Bool),
                    _ => None,
                }
            }
            Expr::Call { name, args, .. } => {
                let (params, ret) = self.sigs.get(name.as_str())?.clone();
                if params.len() != args.len() {
                    return None;
                }
                for (a, k) in args.iter().zip(&params) {
                    if self.expr(a)? != *k {
                        return None;
                    }
                }
                Some(ret)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------
// Code generation

/// The compiled functions of one program.
pub struct Native {
    module: Option<JITModule>,
    /// Entry wrappers by function name, as addresses (code owned by `module`).
    entries: HashMap<String, (usize, Vec<K>, K)>,
}

/// Helpers imported into every function.
struct Helpers {
    step: FuncId,
    depth: FuncId,
    arith: FuncId,
    neg: FuncId,
    no_return: FuncId,
}

impl Native {
    /// Compiles every eligible function. Nothing is compiled for a program with none.
    pub fn compile(prog: &Program) -> Result<Native, String> {
        let set = eligible(prog);
        if set.is_empty() {
            return Ok(Native { module: None, entries: HashMap::new() });
        }
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        flags.set("use_colocated_libcalls", "false").map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(|e| format!("native code is not available on this machine: {e}"))?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        jb.symbol("intent_rt_step_limit", rt_step_limit as *const u8);
        jb.symbol("intent_rt_depth_limit", rt_depth_limit as *const u8);
        jb.symbol("intent_rt_arith", rt_arith as *const u8);
        jb.symbol("intent_rt_neg", rt_neg as *const u8);
        jb.symbol("intent_rt_no_return", rt_no_return as *const u8);
        let mut module = JITModule::new(jb);
        let ptr = module.target_config().pointer_type();
        if ptr != types::I64 {
            return Err("native code needs a 64-bit machine".into());
        }

        let helper = |m: &mut JITModule, name: &str, extra: usize| -> Result<FuncId, String> {
            let mut sig = m.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            for _ in 0..extra {
                sig.params.push(AbiParam::new(types::I64));
            }
            m.declare_function(name, Linkage::Import, &sig).map_err(|e| e.to_string())
        };
        let helpers = Helpers {
            step: helper(&mut module, "intent_rt_step_limit", 0)?,
            depth: helper(&mut module, "intent_rt_depth_limit", 0)?,
            arith: helper(&mut module, "intent_rt_arith", 3)?,
            neg: helper(&mut module, "intent_rt_neg", 1)?,
            no_return: helper(&mut module, "intent_rt_no_return", 0)?,
        };

        // Declare every compiled function and its entry wrapper first, so calls can
        // refer to any of them.
        let fns: Vec<&FnDecl> = prog.fns.iter().filter(|f| set.contains(&f.name)).collect();
        let mut ids: HashMap<String, FuncId> = HashMap::new();
        let mut wrappers: HashMap<String, FuncId> = HashMap::new();
        let mut kinds: HashMap<String, (Vec<K>, K)> = HashMap::new();
        for (i, f) in fns.iter().enumerate() {
            let params: Vec<K> = f.params.iter().map(|p| kind_of(&p.ty).expect("eligible")).collect();
            let ret = f.ret.as_ref().and_then(kind_of).expect("eligible");
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            for _ in &params {
                sig.params.push(AbiParam::new(types::I64));
            }
            sig.returns.push(AbiParam::new(types::I64));
            let id = module.declare_function(&format!("f{i}"), Linkage::Local, &sig).map_err(|e| e.to_string())?;
            let mut wsig = module.make_signature();
            wsig.params.push(AbiParam::new(types::I64));
            wsig.params.push(AbiParam::new(types::I64));
            wsig.returns.push(AbiParam::new(types::I64));
            let wid = module.declare_function(&format!("w{i}"), Linkage::Local, &wsig).map_err(|e| e.to_string())?;
            ids.insert(f.name.clone(), id);
            wrappers.insert(f.name.clone(), wid);
            kinds.insert(f.name.clone(), (params, ret));
        }

        let mut ctx = module.make_context();
        let mut fctx = FunctionBuilderContext::new();
        for f in &fns {
            let id = ids[&f.name];
            ctx.func.signature = module.declarations().get_function_decl(id).signature.clone();
            ctx.func.name = UserFuncName::user(0, id.as_u32());
            {
                let b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                let g = Gen::new(b, &mut module, &helpers, &ids, &kinds);
                g.function(f)?;
            }
            module.define_function(id, &mut ctx).map_err(|e| format!("native code for '{}': {e:?}", f.name))?;
            module.clear_context(&mut ctx);

            // The wrapper: (ctx, argv) -> the function's result, reading arguments from argv.
            let wid = wrappers[&f.name];
            ctx.func.signature = module.declarations().get_function_decl(wid).signature.clone();
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
            module.define_function(wid, &mut ctx).map_err(|e| format!("native entry for '{}': {e:?}", f.name))?;
            module.clear_context(&mut ctx);
        }
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let mut entries = HashMap::new();
        for f in &fns {
            let p = module.get_finalized_function(wrappers[&f.name]);
            entries.insert(f.name.clone(), (p as usize, kinds[&f.name].0.clone(), kinds[&f.name].1));
        }
        Ok(Native { module: Some(module), entries })
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
    /// including this one. Ok(None) if `name` is not compiled or the arguments do not
    /// fit its parameters, which the caller treats as "run it on the interpreter".
    pub fn call(&self, name: &str, ctx: &mut Ctx, args: &[crate::interp::value::Value]) -> Option<Result<crate::interp::value::Value, Failure>> {
        use crate::interp::value::Value;
        let (ptr, params, ret) = self.entries.get(name)?;
        if params.len() != args.len() {
            return None;
        }
        let mut argv: Vec<i64> = Vec::with_capacity(args.len());
        for (a, k) in args.iter().zip(params) {
            match (a, k) {
                (Value::Int(n), K::Int) => argv.push(*n),
                (Value::Bool(b), K::Bool) => argv.push(*b as i64),
                _ => return None,
            }
        }
        ctx.failed = 0;
        ctx.failure = None;
        // SAFETY: `ptr` is a finalized wrapper with signature (ctx, argv) -> i64,
        // generated above; `argv` holds exactly as many i64 as the function takes; the
        // module that owns the code lives as long as `self`.
        let f: extern "C" fn(*mut Ctx, *const i64) -> i64 = unsafe { std::mem::transmute::<usize, extern "C" fn(*mut Ctx, *const i64) -> i64>(*ptr) };
        let r = f(ctx as *mut Ctx, argv.as_ptr());
        if ctx.failed != 0 {
            return Some(Err(ctx.failure.take().unwrap_or(Failure {
                kind: FailKind::Internal,
                message: "native code failed without saying why".into(),
            })));
        }
        Some(Ok(match ret {
            K::Int => Value::Int(r),
            K::Bool => Value::Bool(r != 0),
        }))
    }
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

struct Gen<'b, 'm> {
    b: FunctionBuilder<'b>,
    module: &'m mut JITModule,
    helpers: &'m Helpers,
    ids: &'m HashMap<String, FuncId>,
    kinds: &'m HashMap<String, (Vec<K>, K)>,
    ctx: ir::Value,
    scopes: Vec<HashMap<String, Variable>>,
    /// The block each enclosing loop's `break` jumps to.
    exits: Vec<ir::Block>,
}

impl<'b, 'm> Gen<'b, 'm> {
    fn new(
        b: FunctionBuilder<'b>,
        module: &'m mut JITModule,
        helpers: &'m Helpers,
        ids: &'m HashMap<String, FuncId>,
        kinds: &'m HashMap<String, (Vec<K>, K)>,
    ) -> Gen<'b, 'm> {
        Gen { b, module, helpers, ids, kinds, ctx: ir::Value::from_u32(0), scopes: Vec::new(), exits: Vec::new() }
    }

    fn function(mut self, f: &FnDecl) -> Result<(), String> {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        let params: Vec<ir::Value> = self.b.block_params(entry).to_vec();
        self.ctx = params[0];
        let mut first = HashMap::new();
        for (p, v) in f.params.iter().zip(&params[1..]) {
            let var = self.b.declare_var(types::I64);
            self.b.def_var(var, *v);
            first.insert(p.name.clone(), var);
        }
        self.scopes.push(first);
        let falls = self.block(&f.body)?;
        if falls {
            // The checker guarantees a typed function returns; kept for parity.
            self.helper_call(self.helpers.no_return, &[]);
            self.fail_return();
        }
        self.b.seal_all_blocks();
        let cfg = self.module.target_config();
        self.b.finalize(cfg);
        Ok(())
    }

    fn helper_call(&mut self, id: FuncId, extra: &[ir::Value]) {
        let fref = self.module.declare_func_in_func(id, self.b.func);
        let mut args = vec![self.ctx];
        args.extend_from_slice(extra);
        self.b.ins().call(fref, &args);
    }

    /// Returns 0 from the function: the failure is already recorded in Ctx.
    fn fail_return(&mut self) {
        let z = self.b.ins().iconst(types::I64, 0);
        self.b.ins().return_(&[z]);
    }

    /// Branches to a new block that records a failure and returns when `cond` is set;
    /// continues in a new block otherwise.
    fn fail_if(&mut self, cond: ir::Value, helper: FuncId, extra: &[ir::Value]) {
        let bad = self.b.create_block();
        let ok = self.b.create_block();
        self.b.ins().brif(cond, bad, &[], ok, &[]);
        self.b.switch_to_block(bad);
        self.helper_call(helper, extra);
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
        self.fail_if(over, self.helpers.step, &[]);
    }

    /// Compiles a block in a new scope. Returns whether control can fall out of it.
    fn block(&mut self, body: &[Stmt]) -> Result<bool, String> {
        self.scopes.push(HashMap::new());
        let mut falls = true;
        for s in body {
            if !self.stmt(s)? {
                falls = false;
                break;
            }
        }
        self.scopes.pop();
        Ok(falls)
    }

    fn var(&self, name: &str) -> Result<Variable, String> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied()).ok_or(format!("native code: unknown variable '{name}'"))
    }

    /// Compiles one statement. Returns whether control continues after it.
    fn stmt(&mut self, s: &Stmt) -> Result<bool, String> {
        self.tick();
        match s {
            Stmt::Let { name, value, .. } => {
                let v = self.expr(value)?;
                let var = self.b.declare_var(types::I64);
                self.b.def_var(var, v);
                self.scopes.last_mut().expect("a scope").insert(name.clone(), var);
                Ok(true)
            }
            Stmt::Set { name, value, .. } => {
                let v = self.expr(value)?;
                let var = self.var(name)?;
                self.b.def_var(var, v);
                Ok(true)
            }
            Stmt::Expr(e) => {
                self.expr(e)?;
                Ok(true)
            }
            Stmt::Return(Some(e), _) => {
                let v = self.expr(e)?;
                self.b.ins().return_(&[v]);
                Ok(false)
            }
            Stmt::Break(_) => {
                let exit = *self.exits.last().ok_or("native code: `break` outside a loop")?;
                self.b.ins().jump(exit, &[]);
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
                self.exits.push(exit);
                let falls = self.block(body)?;
                self.exits.pop();
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
                self.exits.push(exit);
                let falls = self.block(body)?;
                self.exits.pop();
                if falls {
                    self.b.ins().jump(head, &[]);
                }
                self.b.switch_to_block(exit);
                Ok(true)
            }
            Stmt::Return(None, _) | Stmt::Fail(..) | Stmt::For { .. } => Err("native code: statement not supported".into()),
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<ir::Value, String> {
        match e {
            Expr::Int(n, _) => {
                let n = i64::try_from(*n).map_err(|_| "native code: literal out of range".to_string())?;
                Ok(self.b.ins().iconst(types::I64, n))
            }
            Expr::Bool(v, _) => Ok(self.b.ins().iconst(types::I64, *v as i64)),
            Expr::Var(n, _) => {
                let var = self.var(n)?;
                Ok(self.b.use_var(var))
            }
            Expr::Arith { op, left, right, .. } => {
                let l = self.expr(left)?;
                let r = self.expr(right)?;
                let code = self.b.ins().iconst(types::I64, arith_code(*op));
                match op {
                    ArithOp::Add | ArithOp::Sub | ArithOp::Mul => {
                        let (v, over) = match op {
                            ArithOp::Add => self.b.ins().sadd_overflow(l, r),
                            ArithOp::Sub => self.b.ins().ssub_overflow(l, r),
                            _ => self.b.ins().smul_overflow(l, r),
                        };
                        self.fail_if(over, self.helpers.arith, &[code, l, r]);
                        Ok(v)
                    }
                    ArithOp::Div => {
                        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, r, 0);
                        let min = self.b.ins().icmp_imm_s(IntCC::Equal, l, i64::MIN);
                        let minus1 = self.b.ins().icmp_imm_s(IntCC::Equal, r, -1);
                        let both = self.b.ins().band(min, minus1);
                        let bad = self.b.ins().bor(zero, both);
                        self.fail_if(bad, self.helpers.arith, &[code, l, r]);
                        Ok(self.b.ins().sdiv(l, r))
                    }
                }
            }
            Expr::Neg(inner, _) => {
                let v = self.expr(inner)?;
                let min = self.b.ins().icmp_imm_s(IntCC::Equal, v, i64::MIN);
                self.fail_if(min, self.helpers.neg, &[v]);
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
                let l = self.expr(left)?;
                let r = self.expr(right)?;
                let cc = match op {
                    CmpOp::Eq => IntCC::Equal,
                    CmpOp::Ne => IntCC::NotEqual,
                    CmpOp::Lt => IntCC::SignedLessThan,
                    CmpOp::Le => IntCC::SignedLessThanOrEqual,
                    CmpOp::Gt => IntCC::SignedGreaterThan,
                    CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                };
                let c = self.b.ins().icmp(cc, l, r);
                Ok(self.b.ins().uextend(types::I64, c))
            }
            Expr::Call { name, args, .. } => {
                let mut vals = vec![self.ctx];
                for a in args {
                    vals.push(self.expr(a)?);
                }
                if !self.kinds.contains_key(name) {
                    return Err(format!("native code: '{name}' is not compiled"));
                }
                // The interpreter's depth check, then depth + 1 around the call.
                let flags = MemFlagsData::trusted();
                let depth = self.b.ins().load(types::I64, flags, self.ctx, OFF_DEPTH);
                let max = self.b.ins().load(types::I64, flags, self.ctx, OFF_MAX_DEPTH);
                let deep = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, depth, max);
                self.fail_if(deep, self.helpers.depth, &[]);
                let up = self.b.ins().iadd_imm_s(depth, 1);
                self.b.ins().store(flags, up, self.ctx, OFF_DEPTH);
                let callee = self.module.declare_func_in_func(self.ids[name], self.b.func);
                let call = self.b.ins().call(callee, &vals);
                let r = self.b.inst_results(call)[0];
                self.b.ins().store(flags, depth, self.ctx, OFF_DEPTH);
                let failed = self.b.ins().load(types::I64, flags, self.ctx, OFF_FAILED);
                let (bad, ok) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(failed, bad, &[], ok, &[]);
                self.b.switch_to_block(bad);
                self.fail_return();
                self.b.switch_to_block(ok);
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
        let c = Ctx { steps: 0, max_steps: 0, depth: 0, max_depth: 0, failed: 0, failure: None };
        let base = &c as *const Ctx as usize;
        assert_eq!(&c.steps as *const u64 as usize - base, OFF_STEPS as usize);
        assert_eq!(&c.max_steps as *const u64 as usize - base, OFF_MAX_STEPS as usize);
        assert_eq!(&c.depth as *const u64 as usize - base, OFF_DEPTH as usize);
        assert_eq!(&c.max_depth as *const u64 as usize - base, OFF_MAX_DEPTH as usize);
        assert_eq!(&c.failed as *const u64 as usize - base, OFF_FAILED as usize);
    }
}
