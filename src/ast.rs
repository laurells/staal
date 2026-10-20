// Abstract syntax tree for the Staal v0.1 subset.

use crate::lexer::Pos;

/// An enum's variants, in declaration order, each with its fields in declaration order.
pub type Variants = Vec<(String, Vec<(String, Ty)>)>;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Named(String),
    Proposed(Box<Ty>),
    /// `List<T, N>`: a list of at most N values of type T.
    List(Box<Ty>, u64),
    /// `Optional<T>`: a T, or nothing (`none`).
    Optional(Box<Ty>),
    /// `(A, B, ...)`: a fixed number of values, of these types, in this order.
    Tuple(Vec<Ty>),
    /// `Map<K, V, N>`: at most N values of type V, each under a different key of type
    /// K (Text or Int), in key order.
    Map(Box<Ty>, Box<Ty>, u64),
    /// `Set<T, N>`: at most N different values of type T (Text or Int), in order.
    Set(Box<Ty>, u64),
    /// `fn(A, B) -> R`: a function value. Function values only compute: they never
    /// act, ask a model or hold authority (see `Expr::Lambda`).
    Func(Vec<Ty>, Box<Ty>),
    /// A capability, e.g. `cap payments.refund(max: 500 USD)`, with its limits.
    Cap { path: Vec<String>, limits: CapLimits },
}

impl Ty {
    /// The type of `none` before its use says what it may hold: `Optional<?>`, written
    /// with an empty name no program can spell. Its only value is `none`, so it fits
    /// any optional type (`fits`).
    pub fn none() -> Ty {
        Ty::Optional(Box::new(Ty::Named(String::new())))
    }

    fn is_unknown(&self) -> bool {
        matches!(self, Ty::Named(n) if n.is_empty())
    }

    /// Whether `none`'s type, which says nothing of what it may hold, is anywhere in this.
    pub fn has_unknown(&self) -> bool {
        match self {
            Ty::Named(n) => n.is_empty(),
            Ty::Proposed(t) | Ty::List(t, _) | Ty::Optional(t) => t.has_unknown(),
            Ty::Tuple(ts) => ts.iter().any(Ty::has_unknown),
            Ty::Map(k, v, _) => k.has_unknown() || v.has_unknown(),
            Ty::Set(t, _) => t.has_unknown(),
            Ty::Func(ps, r) => ps.iter().any(Ty::has_unknown) || r.has_unknown(),
            Ty::Cap { .. } => false,
        }
    }

    /// The one type both `self` and `other` fit, if any: equal types, or the same shape
    /// where one side has `none`'s type and the other an optional type.
    pub fn unify(&self, other: &Ty) -> Option<Ty> {
        match (self, other) {
            (Ty::Optional(a), Ty::Optional(b)) if a.is_unknown() => Some(Ty::Optional(b.clone())),
            (Ty::Optional(a), Ty::Optional(b)) if b.is_unknown() => Some(Ty::Optional(a.clone())),
            (Ty::Optional(a), Ty::Optional(b)) => Some(Ty::Optional(Box::new(a.unify(b)?))),
            (Ty::List(a, n), Ty::List(b, m)) if n == m => Some(Ty::List(Box::new(a.unify(b)?), *n)),
            (Ty::Proposed(a), Ty::Proposed(b)) => Some(Ty::Proposed(Box::new(a.unify(b)?))),
            (Ty::Map(ka, va, n), Ty::Map(kb, vb, m)) if n == m && ka == kb => Some(Ty::Map(ka.clone(), Box::new(va.unify(vb)?), *n)),
            (Ty::Tuple(a), Ty::Tuple(b)) if a.len() == b.len() => {
                Some(Ty::Tuple(a.iter().zip(b).map(|(x, y)| x.unify(y)).collect::<Option<Vec<_>>>()?))
            }
            _ => (self == other).then(|| self.clone()),
        }
    }

    /// Whether a value of type `self` is also a value of type `to`: the same type, or
    /// `none`'s type where `to` has an optional type.
    pub fn fits(&self, to: &Ty) -> bool {
        self.unify(to).as_ref() == Some(to)
    }

    /// Like `fits`, and a list, map or set may also go where a larger one is allowed:
    /// a value of `List<T, 3>` is a value of `List<T, 10>`.
    pub fn widens(&self, to: &Ty) -> bool {
        match (self, to) {
            (Ty::List(a, n), Ty::List(b, m)) | (Ty::Set(a, n), Ty::Set(b, m)) => n <= m && a.widens(b),
            (Ty::Map(ka, va, n), Ty::Map(kb, vb, m)) => n <= m && ka == kb && va.widens(vb),
            (Ty::Optional(a), Ty::Optional(b)) if !a.is_unknown() => a.widens(b),
            (Ty::Tuple(a), Ty::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.widens(y)),
            _ => self.fits(to),
        }
    }
}

/// The limits on a capability. Every limit is optional; a capability with none is
/// unlimited. All of them hold together, and through every narrowed copy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CapLimits {
    /// `max: 500 USD`: the most money that may be spent through it in total.
    pub max: Option<Limit>,
    /// `each: 50 USD`: the most money one action may spend.
    pub each: Option<Limit>,
    /// `uses: 10`: how many actions may be attempted through it in total.
    pub uses: Option<u64>,
    /// `only: ["a", "*@b.org"]`: the targets it may act on (see `pattern`).
    pub only: Option<Vec<String>>,
}

impl CapLimits {
    pub fn is_empty(&self) -> bool {
        *self == CapLimits::default()
    }
}

impl std::fmt::Display for CapLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if let Some(m) = &self.max {
            parts.push(format!("max: {m}"));
        }
        if let Some(e) = &self.each {
            parts.push(format!("each: {e}"));
        }
        if let Some(u) = self.uses {
            parts.push(format!("uses: {u}"));
        }
        if let Some(o) = &self.only {
            let items: Vec<String> = o.iter().map(|p| format!("{p:?}")).collect();
            parts.push(format!("only: [{}]", items.join(", ")));
        }
        if parts.is_empty() { Ok(()) } else { write!(f, "({})", parts.join(", ")) }
    }
}

/// An upper bound on a capability, e.g. `500 USD`. `unit` is None for a bare number.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub amount: u64,
    pub unit: Option<String>,
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.unit {
            Some(u) => write!(f, "{} {u}", self.amount),
            None => write!(f, "{}", self.amount),
        }
    }
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Named(n) => write!(f, "{n}"),
            Ty::Proposed(t) => write!(f, "Proposed<{t}>"),
            Ty::List(t, n) => write!(f, "List<{t}, {n}>"),
            Ty::Optional(t) if t.is_unknown() => write!(f, "none"),
            Ty::Optional(t) => write!(f, "Optional<{t}>"),
            Ty::Tuple(ts) => write!(f, "({})", ts.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")),
            Ty::Map(k, v, n) => write!(f, "Map<{k}, {v}, {n}>"),
            Ty::Set(t, n) => write!(f, "Set<{t}, {n}>"),
            Ty::Func(ps, r) => write!(f, "fn({}) -> {r}", ps.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")),
            Ty::Cap { path, limits } => write!(f, "cap {}{limits}", path.join(".")),
        }
    }
}

/// `const NAME: T = value`: a value known when the program is checked. Before the
/// checker runs, every use is replaced by the value's expression (`crate::consts`), so
/// a constant behaves exactly like the literal it stands for.
#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub public: bool,
    pub name: String,
    pub ty: Option<Ty>,
    pub value: Expr,
    pub pos: Pos,
}

/// `enum Status { Pending, Paid { amount: Money } }`: a value is exactly one of the
/// variants, each with its own fields (or none).
#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub public: bool,
    pub name: String,
    pub variants: Vec<Variant>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
    pub pos: Pos,
}

/// What one arm of a `match` accepts.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// `Paid { amount, fee: f }` or `Paid { amount, .. }`: the variant, and each field
    /// bound to a name (`amount` binds `amount`). Without `..` every field is bound.
    Variant { name: String, binds: Vec<(String, String)>, rest: bool, pos: Pos },
    /// `some(x)`: an optional value that holds one, bound to `x`.
    Present { bind: String, pos: Pos },
    /// `none`: an optional value that holds nothing.
    Absent(Pos),
    /// `_`: every variant no earlier arm accepted.
    Any(Pos),
}

#[derive(Debug, Clone)]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub struct TypeDecl {
    /// `pub type ...`: other modules may use it.
    pub public: bool,
    pub name: String,
    pub fields: Vec<(String, Ty)>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    /// `name: T @trusted` — callers may only pass trusted values here.
    pub trusted: bool,
    /// `@target` / `@amount`, on a parameter of a capability action only: the value the
    /// program's `only` and money limits are checked against (RUNTIME.md section 13).
    pub target: bool,
    pub amount: bool,
    /// `@blind` (whole value, stored as one empty path) or `@blind(customer.email, ...)`:
    /// paths inside this parameter that must never reach a model, here or in anything
    /// this function passes them to.
    pub blind: Vec<Vec<String>>,
    /// `name: T = value`: what a call that leaves this parameter out passes. A value
    /// of the same form as a constant's, known when the program is checked.
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    /// `pub fn ...`: other modules may call it.
    pub public: bool,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Ty>,
    /// `-> T @trusted` — every returned value must be trusted.
    pub ret_trusted: bool,
    pub budget: Option<Budget>,
    /// `fails E`: the record or enum type its `fail(...)` raises, which a caller's
    /// `try` can read back as `outcome.failure`.
    pub fails: Option<Ty>,
    /// `fn first<T>(xs: List<T, 10>) -> Optional<T>`: its type parameters. A generic
    /// function is checked once with each parameter as a type of which nothing is
    /// known, and run as one copy per set of types it is called with (`first<Int>`),
    /// which the compiler makes and checks like any function.
    pub generics: Vec<String>,
    /// `<T: Priced + Named>`: each type parameter and an interface it must satisfy.
    pub bounds: Vec<(String, String)>,
    /// `fn Charge.total(self) -> Money`: a method of the record or enum `Charge`,
    /// called as `c.total()`. Its first parameter, `self`, is the value it is called
    /// on. Its name is `Charge::total` (qualified by its module like any name).
    pub method_of: Option<String>,
    pub body: Vec<Stmt>,
    pub pos: Pos,
}

/// `budget(calls: N, tokens: N)` — either part may be omitted.
#[derive(Debug, Clone)]
pub struct Budget {
    pub calls: Option<u64>,
    pub tokens: Option<u64>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, ty: Option<Ty>, value: Expr, pos: Pos },
    Expr(Expr),
    /// `return value`, or a bare `return` in a function that returns nothing
    Return(Option<Expr>, Pos),
    /// `repeat N { ... }` — runs the body at most N times
    Repeat { count: u64, body: Vec<Stmt>, pos: Pos },
    /// `loop { ... }` — no bound on iterations
    Loop { body: Vec<Stmt>, pos: Pos },
    /// `if cond { ... } else { ... }`; `else if` is stored as an else block holding one If.
    If { cond: Expr, then: Vec<Stmt>, els: Vec<Stmt>, pos: Pos },
    /// `break` — leave the innermost `repeat`, `loop` or `for`
    Break(Pos),
    /// `continue`: on to the next iteration of the innermost loop.
    Continue(Pos),
    /// `match value { Pattern => { ... } ... }`: runs the first arm whose pattern accepts
    /// the value's variant. The arms cover every variant (E109).
    Match { value: Expr, arms: Vec<Arm>, pos: Pos },
    /// `for x in xs { ... }` — once per element of a bounded list
    For { var: String, list: Expr, body: Vec<Stmt>, pos: Pos },
    /// `set name = value` — give a variable declared with `let` a new value
    Set { name: String, value: Expr, pos: Pos },
    /// `fail(message)` — stop with a failure of kind `action` that callers may catch
    Fail(Expr, Pos),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl std::fmt::Display for ArithOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "/",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogicOp {
    And,
    Or,
}

/// Functions built into the language. Their names are reserved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Builtin {
    Len,
    Contains,
    StartsWith,
    EndsWith,
    /// `header(name, value)`: an HTTP header; the value may be a Secret.
    Header,
    /// `json(x)`: x encoded as JSON text.
    Json,
    /// `fill_secret(template, key)`: the template with `{secret}` replaced by the key,
    /// as a new Secret (for a request body or header that embeds the key).
    FillSecret,
    /// `replace(text, from, to)`, `lower(text)`, `trim(text)`
    Replace,
    Lower,
    Trim,
    /// `form(record)`: a record of Text, Int and Bool fields, form-encoded
    /// (application/x-www-form-urlencoded), for APIs that take forms.
    Form,
    /// `minor_units(money)`: the amount in minor units (cents); `currency(money)`: its code.
    MinorUnits,
    Currency,
    /// `url_segment(text)`: the text percent-encoded to be one path segment of a URL,
    /// so data placed in a URL can never add a segment, a query or a fragment. Fails for
    /// "", "." and "..", which a URL would treat as no segment or as moving up a level.
    UrlSegment,
    /// `to_text(x)`: a Text, Int, Bool or Money as text (`12.50 USD`). Interpolation,
    /// `"\(x)"`, is `"" + to_text(x)`.
    Text,
    /// Maps: `get(m, k)` (an Optional), `put(m, k, v)`, `remove(m, k)`, `keys(m)`,
    /// `values(m)`, `entries(m)` (a list of (key, value) tuples). Sets: `add(s, x)`,
    /// `remove(s, x)`, `items(s)`, and `to_set(xs)` from a list. `put` and `add` take
    /// the collection's capacity as a last argument, which the compiler supplies.
    Get,
    Put,
    Remove,
    Keys,
    Values,
    Entries,
    Add,
    Items,
    ToSet,
    /// Text: `upper(t)`; `slice(t, start, end)` (characters, also on lists);
    /// `index_of(t, part)` (an `Optional<Int>`, also on lists); `split(t, sep, N)` (a
    /// `List<Text, N>`, N a literal); `join(xs, sep)`; `to_int(t)` (an `Optional<Int>`).
    Upper,
    Slice,
    IndexOf,
    Split,
    Join,
    ToInt,
    /// Numbers: `abs(n)`, `min(a, b)`, `max(a, b)` (Int or Money), `rem(a, b)`.
    Abs,
    Min,
    Max,
    Rem,
    /// Lists: `append(xs, x)`, which takes the list's capacity as a last argument (the
    /// compiler supplies it, as for `put`); `reverse(xs)`; `sort(xs)` (of Int, Text or
    /// Money); `sum(xs)` (of Int); `concat(a, b)`.
    Append,
    Reverse,
    Sort,
    Sum,
    Concat,
    /// Lists with a function value, called on each item in order: `map(xs, f)`,
    /// `filter(xs, keep)`, `any(xs, test)` and `all(xs, test)` (which stop at the first
    /// item that decides), `count(xs, test)`, and `fold(xs, start, f)`.
    Map,
    Filter,
    Any,
    All,
    Count,
    Fold,
    /// Hashing and encoding: `sha256(t)` (lowercase hex of the SHA-256 of t's UTF-8
    /// bytes); `base64_encode(t)`, `base64_decode(t)` (standard alphabet, padded; an
    /// `Optional<Text>`); `hex_encode(t)`, `hex_decode(t)` (lowercase; an
    /// `Optional<Text>`).
    Sha256,
    Base64Encode,
    Base64Decode,
    HexEncode,
    HexDecode,
    /// Dates and times, on an instant written as an Int of milliseconds since
    /// 1970-01-01T00:00:00Z: `format_time(t)`, `parse_time(text)` (an `Optional<Int>`),
    /// `make_time(year, month, day, hour, minute, second)`, `date_parts(t)` (year,
    /// month, day), `time_parts(t)` (hour, minute, second, millisecond), `weekday(t)`
    /// (1 Monday to 7 Sunday).
    FormatTime,
    ParseTime,
    MakeTime,
    DateParts,
    TimeParts,
    Weekday,
    /// What differs between runs, each an external operation in the journal, so a
    /// resumed run replays what the first saw: `print(text)` writes a line to standard
    /// output; `now()` is the current time (an Int of milliseconds since 1970 in UTC);
    /// `random_int(lo, hi)` is an Int from lo to hi inclusive, from the operating
    /// system's random source.
    Print,
    Now,
    RandomInt,
}

impl Builtin {
    /// Whether it is an external operation, journaled (RUNTIME.md section 11).
    pub fn is_effect(self) -> bool {
        matches!(self, Builtin::Print | Builtin::Now | Builtin::RandomInt)
    }

    /// Whether it calls a function value it is given.
    pub fn calls_function(self) -> bool {
        matches!(self, Builtin::Map | Builtin::Filter | Builtin::Any | Builtin::All | Builtin::Count | Builtin::Fold)
    }

    pub fn from_name(n: &str) -> Option<Builtin> {
        match n {
            "len" => Some(Builtin::Len),
            "contains" => Some(Builtin::Contains),
            "starts_with" => Some(Builtin::StartsWith),
            "ends_with" => Some(Builtin::EndsWith),
            "header" => Some(Builtin::Header),
            "json" => Some(Builtin::Json),
            "fill_secret" => Some(Builtin::FillSecret),
            "replace" => Some(Builtin::Replace),
            "lower" => Some(Builtin::Lower),
            "trim" => Some(Builtin::Trim),
            "form" => Some(Builtin::Form),
            "minor_units" => Some(Builtin::MinorUnits),
            "currency" => Some(Builtin::Currency),
            "url_segment" => Some(Builtin::UrlSegment),
            "to_text" => Some(Builtin::Text),
            "get" => Some(Builtin::Get),
            "put" => Some(Builtin::Put),
            "remove" => Some(Builtin::Remove),
            "keys" => Some(Builtin::Keys),
            "values" => Some(Builtin::Values),
            "entries" => Some(Builtin::Entries),
            "add" => Some(Builtin::Add),
            "items" => Some(Builtin::Items),
            "to_set" => Some(Builtin::ToSet),
            "upper" => Some(Builtin::Upper),
            "slice" => Some(Builtin::Slice),
            "index_of" => Some(Builtin::IndexOf),
            "split" => Some(Builtin::Split),
            "join" => Some(Builtin::Join),
            "to_int" => Some(Builtin::ToInt),
            "abs" => Some(Builtin::Abs),
            "min" => Some(Builtin::Min),
            "max" => Some(Builtin::Max),
            "rem" => Some(Builtin::Rem),
            "append" => Some(Builtin::Append),
            "reverse" => Some(Builtin::Reverse),
            "sort" => Some(Builtin::Sort),
            "sum" => Some(Builtin::Sum),
            "concat" => Some(Builtin::Concat),
            "map" => Some(Builtin::Map),
            "filter" => Some(Builtin::Filter),
            "any" => Some(Builtin::Any),
            "all" => Some(Builtin::All),
            "count" => Some(Builtin::Count),
            "fold" => Some(Builtin::Fold),
            "sha256" => Some(Builtin::Sha256),
            "base64_encode" => Some(Builtin::Base64Encode),
            "base64_decode" => Some(Builtin::Base64Decode),
            "hex_encode" => Some(Builtin::HexEncode),
            "hex_decode" => Some(Builtin::HexDecode),
            "format_time" => Some(Builtin::FormatTime),
            "parse_time" => Some(Builtin::ParseTime),
            "make_time" => Some(Builtin::MakeTime),
            "date_parts" => Some(Builtin::DateParts),
            "time_parts" => Some(Builtin::TimeParts),
            "weekday" => Some(Builtin::Weekday),
            "print" => Some(Builtin::Print),
            "now" => Some(Builtin::Now),
            "random_int" => Some(Builtin::RandomInt),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Len => "len",
            Builtin::Contains => "contains",
            Builtin::StartsWith => "starts_with",
            Builtin::EndsWith => "ends_with",
            Builtin::Header => "header",
            Builtin::Json => "json",
            Builtin::FillSecret => "fill_secret",
            Builtin::Replace => "replace",
            Builtin::Lower => "lower",
            Builtin::Trim => "trim",
            Builtin::Form => "form",
            Builtin::MinorUnits => "minor_units",
            Builtin::Currency => "currency",
            Builtin::UrlSegment => "url_segment",
            Builtin::Text => "to_text",
            Builtin::Get => "get",
            Builtin::Put => "put",
            Builtin::Remove => "remove",
            Builtin::Keys => "keys",
            Builtin::Values => "values",
            Builtin::Entries => "entries",
            Builtin::Add => "add",
            Builtin::Items => "items",
            Builtin::ToSet => "to_set",
            Builtin::Upper => "upper",
            Builtin::Slice => "slice",
            Builtin::IndexOf => "index_of",
            Builtin::Split => "split",
            Builtin::Join => "join",
            Builtin::ToInt => "to_int",
            Builtin::Abs => "abs",
            Builtin::Min => "min",
            Builtin::Max => "max",
            Builtin::Rem => "rem",
            Builtin::Append => "append",
            Builtin::Reverse => "reverse",
            Builtin::Sort => "sort",
            Builtin::Sum => "sum",
            Builtin::Concat => "concat",
            Builtin::Map => "map",
            Builtin::Filter => "filter",
            Builtin::Any => "any",
            Builtin::All => "all",
            Builtin::Count => "count",
            Builtin::Fold => "fold",
            Builtin::Sha256 => "sha256",
            Builtin::Base64Encode => "base64_encode",
            Builtin::Base64Decode => "base64_decode",
            Builtin::HexEncode => "hex_encode",
            Builtin::HexDecode => "hex_decode",
            Builtin::FormatTime => "format_time",
            Builtin::ParseTime => "parse_time",
            Builtin::MakeTime => "make_time",
            Builtin::DateParts => "date_parts",
            Builtin::TimeParts => "time_parts",
            Builtin::Weekday => "weekday",
            Builtin::Print => "print",
            Builtin::Now => "now",
            Builtin::RandomInt => "random_int",
        }
    }

    /// The number of arguments it takes.
    pub fn arity(self) -> usize {
        match self {
            Builtin::Len
            | Builtin::Json
            | Builtin::Lower
            | Builtin::Trim
            | Builtin::Form
            | Builtin::MinorUnits
            | Builtin::Currency
            | Builtin::UrlSegment
            | Builtin::Text
            | Builtin::Keys
            | Builtin::Values
            | Builtin::Entries
            | Builtin::Items
            | Builtin::ToSet
            | Builtin::Upper
            | Builtin::ToInt
            | Builtin::Abs
            | Builtin::Reverse
            | Builtin::Sort
            | Builtin::Sum
            | Builtin::Sha256
            | Builtin::Base64Encode
            | Builtin::Base64Decode
            | Builtin::HexEncode
            | Builtin::HexDecode
            | Builtin::FormatTime
            | Builtin::ParseTime
            | Builtin::DateParts
            | Builtin::TimeParts
            | Builtin::Weekday
            | Builtin::Print => 1,
            Builtin::Now => 0,
            Builtin::Contains
            | Builtin::StartsWith
            | Builtin::EndsWith
            | Builtin::Header
            | Builtin::FillSecret
            | Builtin::Get
            | Builtin::Remove
            | Builtin::Add
            | Builtin::IndexOf
            | Builtin::Join
            | Builtin::Min
            | Builtin::Max
            | Builtin::Rem
            | Builtin::Append
            | Builtin::Concat
            | Builtin::RandomInt
            | Builtin::Map
            | Builtin::Filter
            | Builtin::Any
            | Builtin::All
            | Builtin::Count => 2,
            Builtin::Replace | Builtin::Put | Builtin::Slice | Builtin::Split | Builtin::Fold => 3,
            Builtin::MakeTime => 6,
        }
    }
}

impl Stmt {
    /// The expressions this statement evaluates itself (not those in nested blocks).
    pub fn exprs(&self) -> Vec<&Expr> {
        match self {
            Stmt::Let { value, .. } | Stmt::Set { value, .. } | Stmt::Fail(value, _) => vec![value],
            Stmt::Expr(e) | Stmt::Return(Some(e), _) => vec![e],
            Stmt::If { cond, .. } => vec![cond],
            Stmt::For { list, .. } => vec![list],
            Stmt::Match { value, .. } => vec![value],
            Stmt::Return(None, _) | Stmt::Repeat { .. } | Stmt::Loop { .. } | Stmt::Break(_) | Stmt::Continue(_) => {
                Vec::new()
            }
        }
    }

    /// The blocks nested directly in this statement.
    pub fn blocks(&self) -> Vec<&[Stmt]> {
        match self {
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } | Stmt::For { body, .. } => vec![body],
            Stmt::If { then, els, .. } => vec![then, els],
            Stmt::Match { arms, .. } => arms.iter().map(|a| a.body.as_slice()).collect(),
            Stmt::Let { .. }
            | Stmt::Set { .. }
            | Stmt::Fail(..)
            | Stmt::Expr(_)
            | Stmt::Return(..)
            | Stmt::Break(_)
            | Stmt::Continue(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl std::fmt::Display for CmpOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    Var(String, Pos),
    Str(String, Pos),
    Int(u64, Pos),
    Bool(bool, Pos),
    /// `12.50 USD`: an exact amount, checked against its currency when parsed.
    Money(crate::interp::money::Money, Pos),
    /// `base.field`, e.g. `order.total` or `ticket.customer.email`
    Field { base: Box<Expr>, field: String, pos: Pos },
    /// `left op right`, e.g. `count <= 3`
    Cmp { op: CmpOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `left + right` and the other arithmetic operators
    Arith { op: ArithOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `-x`
    Neg(Box<Expr>, Pos),
    /// `left and right`, `left or right`; the right side runs only if needed
    Logic { op: LogicOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `not x`
    Not(Box<Expr>, Pos),
    /// `len(x)` and the other built-in functions
    Builtin { f: Builtin, args: Vec<Expr>, pos: Pos },
    /// `Type { field: value, ... }`
    Record { ty: String, fields: Vec<(String, Expr)>, pos: Pos },
    /// `[a, b, c]`
    List(Vec<Expr>, Pos),
    /// `fn(x: Int) -> Int { return x * k }`: a function value, which may read the
    /// variables around it (`k`): each is copied when the value is made. Its body only
    /// computes: no action, model call, `done`, `print`, `now` or `random_int`, and it
    /// calls only functions that do none of these either; it cannot read a capability,
    /// a secret, proof or an unvalidated value. The compiler lifts it into a function of its own, taking what
    /// it reads first (`Expr::Closure`).
    Lambda { params: Vec<(String, Ty)>, ret: Box<Ty>, body: Vec<Stmt>, pos: Pos },
    /// A function value: function `name` with `captures` given as its first arguments.
    /// A named function used as a value is one with no captures.
    Closure { name: String, captures: Vec<Expr>, pos: Pos },
    /// `f(args)` where `f` is a variable holding a function value.
    CallValue { callee: Box<Expr>, args: Vec<Expr>, pos: Pos },
    /// `[k: v, ...]`: a map, or `[:]`, an empty one (which takes its type from a
    /// typed `let`).
    MapLit(Vec<(Expr, Expr)>, Pos),
    /// `(a, b)`: a tuple. Its elements are read as `t.0`, `t.1`, ... (a `Field` whose
    /// name is the position), or by `let (x, y) = t`.
    Tuple(Vec<Expr>, Pos),
    /// `Status.Paid { amount: x }` or `Status.Pending`: a value of an enum. The parser
    /// sees these as a record literal and a field read; module resolution, which knows
    /// which names are enums, makes them this.
    Variant { ty: String, variant: String, fields: Vec<(String, Expr)>, pos: Pos },
    /// `some(x)`: an optional value holding x.
    SomeOf(Box<Expr>, Pos),
    /// `none`: an optional value holding nothing. Its type comes from where it is used.
    NoneLit(Pos),
    /// `xs[i]`
    Index { base: Box<Expr>, index: Box<Expr>, pos: Pos },
    /// `name(args)` — a call to a function defined in the program. `args` are in the
    /// order written, which is the order they are evaluated in. `slots` gives the
    /// parameter each one is for; empty means in order (`crate::calls` fills it in for
    /// named arguments and defaults).
    Call { name: String, args: Vec<Expr>, slots: Vec<usize>, pos: Pos },
    /// `base.method(args)` where `base` is not a plain variable (a plain variable's is
    /// parsed as an `Action`). The checker resolves it by `base`'s type to a call of
    /// that type's method, before any other analysis sees it.
    Method { base: Box<Expr>, method: String, args: Vec<Expr>, pos: Pos },
    /// `name: value` among a call's arguments. Only the parser makes one, and
    /// `crate::calls` replaces every one before checking.
    Named { name: String, value: Box<Expr>, pos: Pos },
    /// `propose(T, instruct: E, data: [E, ...], blind_to: [path, ...], tokens: N)` — ask a model for a T.
    /// All options are optional. `instruct` must be trusted (E006); `data` may be anything
    /// except what `blind_to` names (E007). `tokens` is the most this one call may use.
    Propose {
        ty: Ty,
        instruct: Option<Box<Expr>>,
        data: Vec<Expr>,
        blind_to: Vec<Expr>,
        tokens: Option<u64>,
        pos: Pos,
    },
    /// `validate x` — turn Proposed<T> into T
    Validate(Box<Expr>, Pos),
    /// `parse(T, text)` — JSON text from outside, as an unchecked Proposed<T>
    Parse { ty: Ty, text: Box<Expr>, pos: Pos },
    /// `done(evidence: [e, ...])` — the only way to make a `Done`: proof of completed work.
    Done { evidence: Vec<Expr>, pos: Pos },
    /// `try action` or `try call(...)`: a failure becomes an `Outcome` value
    /// instead of stopping the function.
    Try(Box<Expr>, Pos),
    /// `receiver.method(args)` — an action performed through a capability
    Action { receiver: String, method: String, args: Vec<Expr>, pos: Pos },
}

impl Expr {
    /// The expressions directly inside this one, in evaluation order. Exhaustive on
    /// purpose: a new kind of expression must say here what it contains, so analyses
    /// that walk expressions through this function can never skip part of one.
    pub fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) | Expr::NoneLit(..) => Vec::new(),
            Expr::Field { base, .. } => vec![base],
            Expr::Validate(inner, _)
            | Expr::Try(inner, _)
            | Expr::Neg(inner, _)
            | Expr::Not(inner, _)
            | Expr::SomeOf(inner, _) => vec![inner],
            Expr::Named { value, .. } => vec![value],
            Expr::MapLit(entries, _) => entries.iter().flat_map(|(k, v)| [k, v]).collect(),
            Expr::Method { base, args, .. } => std::iter::once(base.as_ref()).chain(args.iter()).collect(),
            // A lambda's body runs when it is called, not where it is written.
            Expr::Lambda { .. } => Vec::new(),
            Expr::Closure { captures, .. } => captures.iter().collect(),
            Expr::CallValue { callee, args, .. } => std::iter::once(callee.as_ref()).chain(args.iter()).collect(),
            Expr::Parse { text, .. } => vec![text],
            Expr::Cmp { left, right, .. } | Expr::Arith { left, right, .. } | Expr::Logic { left, right, .. } => {
                vec![left, right]
            }
            Expr::Index { base, index, .. } => vec![base, index],
            Expr::Call { args, .. }
            | Expr::Action { args, .. }
            | Expr::Builtin { args, .. }
            | Expr::List(args, _)
            | Expr::Tuple(args, _)
            | Expr::Done { evidence: args, .. } => args.iter().collect(),
            Expr::Record { fields, .. } | Expr::Variant { fields, .. } => fields.iter().map(|(_, v)| v).collect(),
            // Only what the model is given is evaluated; `blind_to` names data, it is not evaluated.
            Expr::Propose { instruct, data, .. } => instruct.iter().map(|b| b.as_ref()).chain(data.iter()).collect(),
        }
    }

    /// Every expression directly inside this one, `blind_to` included, to change them.
    pub fn children_mut(&mut self) -> Vec<&mut Expr> {
        match self {
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) | Expr::NoneLit(..) => Vec::new(),
            Expr::Field { base, .. } => vec![base],
            Expr::Validate(inner, _)
            | Expr::Try(inner, _)
            | Expr::Neg(inner, _)
            | Expr::Not(inner, _)
            | Expr::SomeOf(inner, _) => vec![inner],
            Expr::Named { value, .. } => vec![value],
            Expr::MapLit(entries, _) => entries.iter_mut().flat_map(|(k, v)| [k, v]).collect(),
            Expr::Method { base, args, .. } => std::iter::once(base.as_mut()).chain(args.iter_mut()).collect(),
            Expr::Lambda { .. } => Vec::new(),
            Expr::Closure { captures, .. } => captures.iter_mut().collect(),
            Expr::CallValue { callee, args, .. } => std::iter::once(callee.as_mut()).chain(args.iter_mut()).collect(),
            Expr::Parse { text, .. } => vec![text],
            Expr::Cmp { left, right, .. } | Expr::Arith { left, right, .. } | Expr::Logic { left, right, .. } => {
                vec![left, right]
            }
            Expr::Index { base, index, .. } => vec![base, index],
            Expr::Call { args, .. }
            | Expr::Action { args, .. }
            | Expr::Builtin { args, .. }
            | Expr::List(args, _)
            | Expr::Tuple(args, _)
            | Expr::Done { evidence: args, .. } => args.iter_mut().collect(),
            Expr::Record { fields, .. } | Expr::Variant { fields, .. } => fields.iter_mut().map(|(_, v)| v).collect(),
            Expr::Propose { instruct, data, blind_to, .. } => {
                instruct.iter_mut().map(|b| b.as_mut()).chain(data.iter_mut()).chain(blind_to.iter_mut()).collect()
            }
        }
    }

    pub fn pos(&self) -> Pos {
        match self {
            Expr::Var(_, p)
            | Expr::Str(_, p)
            | Expr::Int(_, p)
            | Expr::Bool(_, p)
            | Expr::Money(_, p)
            | Expr::Validate(_, p)
            | Expr::Try(_, p)
            | Expr::Neg(_, p)
            | Expr::Not(_, p)
            | Expr::SomeOf(_, p)
            | Expr::NoneLit(p)
            | Expr::Tuple(_, p)
            | Expr::MapLit(_, p)
            | Expr::List(_, p) => *p,
            Expr::Arith { pos, .. }
            | Expr::Logic { pos, .. }
            | Expr::Builtin { pos, .. }
            | Expr::Record { pos, .. }
            | Expr::Variant { pos, .. }
            | Expr::Index { pos, .. } => *pos,
            Expr::Cmp { pos, .. } | Expr::Field { pos, .. } | Expr::Done { pos, .. } => *pos,
            Expr::Propose { pos, .. } | Expr::Parse { pos, .. } => *pos,
            Expr::Action { pos, .. } | Expr::Call { pos, .. } | Expr::Named { pos, .. } | Expr::Method { pos, .. } => *pos,
            Expr::Lambda { pos, .. } | Expr::Closure { pos, .. } | Expr::CallValue { pos, .. } => *pos,
        }
    }
}

/// `import a.b`, `import a.b as c`, `import a.b { T, f }`: makes module `a/b.staal`
/// available as `b` (or `c`), and optionally names from it without qualification.
#[derive(Debug, Clone)]
pub struct Import {
    /// The module path, e.g. ["sms", "termii"].
    pub path: Vec<String>,
    /// The name the module is known by in this file.
    pub alias: String,
    /// Names brought in unqualified, with where each was written.
    pub names: Vec<(String, Pos)>,
    pub pos: Pos,
}

/// `capability termii.sms { needs ...  canonical(...) { }  action send(...) { } }`:
/// a capability a library defines (RUNTIME.md section 13). Its actions and its
/// `canonical` function are stored as functions in `Program::fns`, under names with a
/// space in them (`action termii.sms.send`, `canonical termii.sms`) that no program
/// can write, so every analysis covers them and nothing can call them directly.
#[derive(Debug, Clone)]
pub struct CapDecl {
    pub path: Vec<String>,
    /// What the operator must supply: runtime capabilities, secrets, text settings.
    pub needs: Vec<(String, Ty, Pos)>,
    pub actions: Vec<ActionDecl>,
    /// The function that puts a `@target` into canonical form.
    pub canonical: Option<String>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct ActionDecl {
    pub name: String,
    /// The function holding its body; its parameters are the declared ones, then the needs.
    pub fn_name: String,
    /// How many parameters the program passes (the rest are the needs).
    pub declared: usize,
    pub target: Option<usize>,
    pub amount: Option<usize>,
    pub pos: Pos,
}

impl CapDecl {
    /// The capability path of one action, as programs write it: `termii.sms.send`.
    pub fn action_path(&self, action: &str) -> String {
        format!("{}.{action}", self.path.join("."))
    }
}

/// `interface Priced { fn price(self) -> Money }`: methods a type must have to be
/// given for a type parameter bounded by it, `<T: Priced>`. A type satisfies it when
/// it has a method of each name with exactly these parameters (after `self`), result
/// and `fails`. Interfaces are only bounds: calls through them are resolved, in each
/// copy of a generic function, to the concrete type's own method.
#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub public: bool,
    pub name: String,
    pub methods: Vec<InterfaceMethod>,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: String,
    /// The parameters after `self`.
    pub params: Vec<(String, Ty)>,
    pub ret: Option<Ty>,
    pub ret_trusted: bool,
    pub fails: Option<Ty>,
    pub pos: Pos,
}

/// `type Name = T`: another name for type T. Before the checker runs, every use is
/// replaced by T (`crate::aliases`), so an alias is exactly the type it names.
#[derive(Debug, Clone)]
pub struct AliasDecl {
    pub public: bool,
    pub name: String,
    pub ty: Ty,
    pub pos: Pos,
}

#[derive(Debug, Default)]
pub struct Program {
    /// The imports of a single parsed file. Empty after modules are resolved into one
    /// program (see `crate::modules`), where every name is already qualified.
    pub imports: Vec<Import>,
    pub types: Vec<TypeDecl>,
    pub fns: Vec<FnDecl>,
    pub caps: Vec<CapDecl>,
    pub consts: Vec<ConstDecl>,
    pub enums: Vec<EnumDecl>,
    /// Type aliases; after modules are resolved, each holds its type with every alias
    /// in it replaced, and no other part of the program names an alias.
    pub aliases: Vec<AliasDecl>,
    pub interfaces: Vec<InterfaceDecl>,
}
