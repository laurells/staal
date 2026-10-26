# The Staal grammar

The complete syntax of Staal source files (`.staal`), as `staalc` parses it
(`src/lexer.rs`, `src/parser.rs`). What a program means, and which well-formed
programs the checker refuses, is in [GUIDE.md](GUIDE.md) and
[RUNTIME.md](RUNTIME.md). `tests/grammar_test.rs` checks this document against the
compiler: every reserved word, contextual word and built-in function the compiler
knows appears here, and every example here parses (a `staal` block as a file, a
`staal-statements` block as the body of a function, a `staal-types` block one type
a line).

## Notation

    rule   := ...          a rule
    "x"                    the token x, exactly
    a b                    a then b
    a | b                  a or b
    a?                     a, or nothing
    a*                     zero or more a
    a+                     one or more a
    ( ... )                grouping
    -- ...                 a note that is not part of the rule

`IDENT`, `NUM`, `DECIMAL` and `STRING` are the tokens described under
[Tokens](#tokens).

## Tokens

A source file is UTF-8 text. Spaces, tabs, carriage returns and line breaks
separate tokens and are otherwise ignored, with two exceptions: a currency code
must be on the same line as its amount, and a one-line text literal cannot cross a
line break.

**Comments** run from `//` to the end of the line.

**Identifiers** (`IDENT`) start with a letter or `_` and go on with letters, digits
and `_`. Letters are Unicode letters. An identifier that is a reserved word is that
word, not an identifier.

**Reserved words**, which are never identifiers:

    and break budget cap continue done else false fn for if import in let loop
    not or propose pub repeat return set true try type validate

A reserved word may still be a record field's name (`order.type`, `T { in: 1 }`),
since JSON from outside often has such fields.

**Contextual words** have a meaning only where the grammar below says, and are
identifiers everywhere else: `action`, `as`, `canonical`, `capability`, `const`,
`enum`, `evidence`, `fail`, `fails`, `interface`, `match`, `needs`, `none`,
`parallel`, `parse`, `self`, `some` and `_`; the budget limits `calls` and `tokens`; the
options of a propose call `instruct`, `data`, `blind_to` and `tokens`; the capability
limits `max`, `each`, `uses` and `only`; the parameter marks `trusted`, `blind`,
`target` and `amount`; and the type names `List`, `Optional`, `Map`, `Set` and
`Proposed` when an angle bracket follows.

**Numbers.** `NUM` is one or more digits, a whole number from 0 to
18446744073709551615 (a larger one is a lexer error; the checker refuses an `Int`
above 9223372036854775807). `DECIMAL` is digits, a `.` and digits, such as `12.50`;
it is only ever part of money or a tuple position. There is no floating point.

**Text.** `STRING` is one of:

- one line between `"` and `"`;
- several lines between `"""` and `"""`: the opening quotes end their line, the
  closing quotes begin one (after spaces or tabs only), and that indentation is
  removed from every line.

Inside either, a backslash starts an escape: `\"`, `\\`, `\n`, `\t`, `\r`, or
`\u{...}` with one to six hexadecimal digits naming a Unicode character. Any other
escape is an error. `\(expr)` interpolates an expression, which must close on the
same line; `"a \(x) b"` means `"a " + to_text(x) + " b"`.

**Symbols:**

    (  )  {  }  [  ]  ,  :  .  ..  @  ->  =>
    =  ==  !=  <  <=  >  >=  +  -  *  /

A character that is none of these, and starts no other token, is a lexer error.

## Files

    program    := import* decl*
    decl       := "pub"? ( alias | record | enum | interface | const | function )
                | capability                  -- never `pub`: a capability is named by its path

    import     := "import" path ( "as" IDENT )? ( "{" IDENT ( "," IDENT )* ","? "}" )?
    path       := IDENT ( "." IDENT )*

Every import comes before every declaration. `pub` makes a declaration usable from
other modules.

```staal
import util.money
import sms.termii as sms { send, Message }
```

## Declarations

    alias      := "type" IDENT "=" type
    record     := "type" IDENT "{" ( field ":" type ","? )* "}"
    enum       := "enum" IDENT "{" variant ( ","? variant )* ","? "}"
    variant    := IDENT ( "{" ( field ":" type ","? )* "}" )?
    interface  := "interface" IDENT "{" method_sig* "}"
    method_sig := "fn" IDENT "(" "self" ( "," IDENT ":" type )* ","? ")" result? fails?
    const      := "const" IDENT ( ":" type )? "=" expr
    field      := IDENT | reserved word

```staal
type Amount = Money
type Line { sku: Text, qty: Int, price: Money }
enum Payment { Pending, Paid { amount: Money, reference: Text } }
interface Priced { fn price(self) -> Money }
const LIMIT: Int = 3
```

## Functions

    function   := "fn" IDENT ( "." IDENT )? signature block
    signature  := generics? "(" params? ")" result? fails? budget?
    generics   := "<" generic ( "," generic )* ">"
    generic    := IDENT ( ":" bound ( "+" bound )* )?
    bound      := IDENT ( "." IDENT )?
    params     := param ( "," param )* ","?
    param      := IDENT ":" type mark* ( "=" expr )?
    mark       := "@" "trusted"
                | "@" "blind" ( "(" value_path ( "," value_path )* ")" )?
                | "@" "target" | "@" "amount"
    value_path := step ( "." step )*
    step       := field | NUM | DECIMAL         -- `0.1`: positions 0 then 1
    result     := "->" type ( "@" "trusted" )?
    fails      := "fails" type
    budget     := "budget" "(" limit ( "," limit )* ")"
    limit      := ( "calls" | "tokens" ) ":" NUM

`fn Line.total(self) -> Money` declares a method of `Line`: its first parameter is
`self`, written without a type; of the marks, only `@blind` may follow it. Each mark and each budget
limit is given at most once. `= expr` gives a parameter a default.

```staal
fn total(lines: List<Line, 100>, fee: Money = 0.00 NGN) -> Money budget(calls: 0) {
    let sum = fee
    for l in lines {
        set sum = sum + l.price * l.qty
    }
    return sum
}

fn first<T: Priced + Named>(xs: List<T, 10>, who: Text @blind) -> Optional<T> {
    return none
}
```

## Capabilities

    capability := "capability" path "{" need* ( canonical | action )* "}"
    need       := "needs" IDENT ":" type
    canonical  := "canonical" signature block
    action     := "action" IDENT signature block

Every `needs` comes before `canonical` and every `action`; a capability has at most
one `canonical`, and `canonical` and `action` may come in any order after the
needs. Which signatures a capability's functions may have is the checker's to say
(GUIDE.md, "Capabilities from libraries").

```staal
capability sms.termii {
    needs http: cap net.http.post(only: ["https://api.ng.termii.com/*"])
    action send(to: Text @target, text: Text) -> Receipt {
        return http.post("https://api.ng.termii.com/api/sms/send", json(to))
    }
}
```

## Types

    type       := named
                | "List" "<" type "," NUM ">"
                | "Optional" "<" type ">"
                | "Map" "<" type "," type "," NUM ">"
                | "Set" "<" type "," NUM ">"
                | "Proposed" "<" type ">"
                | "(" type "," type ( "," type )* ","? ")"     -- a tuple: two or more
                | "fn" "(" ( type ( "," type )* ","? )? ")" ( "->" type )?
                | "cap" path ( "(" cap_limit ( "," cap_limit )* ")" )?
    named      := IDENT ( "." IDENT )?          -- `m.T`: type T of the module imported as m
    cap_limit  := ( "max" | "each" ) ":" NUM IDENT?
                | "uses" ":" NUM
                | "only" ":" "[" ( STRING ( "," STRING )* ","? )? "]"

A function type without `->` returns nothing. Each capability limit is given at
most once; an `only` pattern must be a valid pattern (GUIDE.md, "Capability
limits").

```staal-types
List<Text, 100>
Map<Text, Int, 50>
(Text, Int)
fn(Int) -> Int
cap payments.refund(max: 500 USD, each: 50 USD, uses: 3)
```

## Statements

    block      := "{" stmt* "}"
    stmt       := "let" IDENT ( ":" type )? "=" expr
                | "let" "(" IDENT "," IDENT ( "," IDENT )* ")" ( ":" type )? "=" expr
                | "set" IDENT "=" expr
                | "return" expr?              -- a bare `return` only right before `}`
                | "fail" "(" expr ")"
                | "if" head block ( "else" ( block | if ) )?
                | "match" head "{" arm+ "}"
                | "for" ( IDENT | "(" IDENT "," IDENT ( "," IDENT )* ")" ) "in" head block
                | "repeat" NUM block
                | "loop" block
                | "break"
                | "continue"
                | expr
    arm        := pattern "=>" block
    pattern    := "_"
                | "none"
                | "some" "(" IDENT ")"
                | IDENT ( "{" ( binding ","? )* ( ".." )? "}" )?
    binding    := field ( ":" IDENT )?
    head       := expr                        -- with no record literal outside brackets

Statements need no separator: each ends where its grammar does. `match` begins a
statement only when the next token is not `.`, `(` or `=`, so `match` can still name
a variable or a capability. `fail` is a statement only when `(` follows.

In a `head` (the expression after `if`, `match` or `for ... in`), a `{` opens the
block, so a record literal there must be inside parentheses or brackets:
`if (p == Point { x: 0, y: 0 }) { ... }`.

```staal-statements
let (count, total) = summary(lines)
match payment {
    Paid { amount, reference: r } => { return amount }
    Pending => { fail("not paid") }
}
for (k, v) in entries(m) {
    if v > 3 { continue } else { break }
}
```

## Expressions

From the loosest binding to the tightest:

    expr       := and ( "or" and )*
    and        := not ( "and" not )*
    not        := "not" not | compare
    compare    := sum ( ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) sum )?   -- never chained
    sum        := product ( ( "+" | "-" ) product )*
    product    := unary ( ( "*" | "/" ) unary )*
    unary      := "-" unary | postfix
    postfix    := atom suffix*
    suffix     := "." field args?              -- a field, or a method call
                | "." NUM | "." DECIMAL        -- a tuple position: `t.0`, `t.0.1`
                | "[" expr "]"                 -- an index
                | args                         -- a call of the function value before it

So `not a == b` is `not (a == b)`, `a + b * c` is `a + (b * c)`, and `-x.y` is
`-(x.y)`.

    atom       := "(" expr ")"
                | "(" expr "," expr ( "," expr )* ","? ")"       -- a tuple
                | "[" "]" | "[" ":" "]"                          -- empty list or set; empty map
                | "[" expr ( "," expr )* ","? "]"                -- a list
                | "[" expr ":" expr ( "," expr ":" expr )* ","? "]"   -- a map
                | "fn" "(" ( IDENT ":" type ( "," IDENT ":" type )* ","? )? ")" ( "->" type )? block
                | "propose" "(" type ( "," propose_opt )* ")"
                | "validate" postfix
                | "parse" "(" type "," expr ")"
                | "parallel" "(" expr "," path ( "," ( IDENT | STRING | NUM | money | "true" | "false" ) )* ")"
                | "try" atom
                | "done" "(" "evidence" ":" list ")"
                | "some" "(" expr ")"
                | "none"
                | "true" | "false"
                | STRING
                | money
                | NUM
                | IDENT args                                      -- a call
                | IDENT "." IDENT args                            -- an action, or an imported function
                | IDENT ( "." IDENT ( "." IDENT )? )? "{" fields? "}"   -- a record or variant
                | IDENT
    fields     := field ":" expr ( "," field ":" expr )* ","?
    propose_opt:= "instruct" ":" expr | "data" ":" list | "blind_to" ":" list | "tokens" ":" NUM
    money      := ( NUM | DECIMAL ) CURRENCY      -- `12.50 NGN`, on one line
    list       := "[" ( expr ( "," expr )* )? "]"
    args       := "(" ( arg ( "," arg )* )? ")"
    arg        := expr | IDENT ":" expr           -- a named argument

`CURRENCY` is three capital letters right after the number on the same line, which
must name a currency the runtime supports (a number followed on its line by any
three capital letters is always money); the amount must have no more decimals than
the currency allows. A `DECIMAL` without a currency is an error. Each `propose` option
is given at most once. In `IDENT args`, an `IDENT` that names a built-in function
(below) calls it.

`IDENT "." IDENT args` is an action when the first name is a capability, and a call
of the module's function when it is an imported module; on any other value it is a
method call. Which a record literal `A.B { ... }` is, a record of type `B` from the
module `A` or variant `B` of the enum `A`, is decided once names are known.

```staal-statements
let fee = 2.50 USD
let quote = validate propose(Quote, instruct: "Price this.", data: [order], tokens: 400)
let sent = try sms.send(to, "Your code is \(code)")
let replies = parallel(tickets, answer, model_budget, "en")
let total = cart.lines[0].total() + adder(3)(4)
let paid = Payment.Paid { amount: fee, reference: "r-1" }
return done(evidence: [sent])
```

## Built-in functions

These names are built-in functions. A program cannot declare a function with one,
and `name(...)` with one of them always calls the built-in:

    len contains starts_with ends_with header json fill_secret replace lower trim form
    minor_units currency url_segment to_text get put remove keys values entries add
    items to_set upper slice index_of split join to_int abs min max rem append
    reverse sort sum concat map filter any all count fold sha256 base64_encode
    base64_decode hex_encode hex_decode format_time parse_time make_time date_parts
    time_parts weekday print now random_int read_line

`parse`, `parallel`, `some` and `none` are part of the grammar above. In
`parallel(list, f, a, b)`, `f` names a declared function (`m.f` one from a module),
not a call of it, and what follows it is names and literals only, so nothing is
evaluated once per task. What each built-in takes
and gives is in RUNTIME.md, sections 2, 4 and 12.
