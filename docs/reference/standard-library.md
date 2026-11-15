# Standard library

Built-in functions are available in every file without an import. Their names are
reserved: a function cannot be declared with one, and a variable with one cannot be
called. Positions in text count characters from 0. A function marked *fails* ends the
run with a failure of kind `value` in the case given, unless the call is in `try`.

What a built-in function returns is trusted only if all its arguments are, and it
reveals everything it is given (E006, E007).

## Text

| Function | Result |
| --- | --- |
| `len(t: Text) -> Int` | The number of characters. |
| `contains(t: Text, part: Text) -> Bool` | Whether `part` occurs in `t`. |
| `starts_with(t: Text, prefix: Text) -> Bool` | |
| `ends_with(t: Text, suffix: Text) -> Bool` | |
| `upper(t: Text) -> Text`, `lower(t: Text) -> Text` | The text in upper or lower case, by Unicode's rules (`ß` becomes `SS`). *Fails* if the result exceeds 16 MiB. |
| `trim(t: Text) -> Text` | `t` without leading and trailing Unicode white space. |
| `replace(t: Text, from: Text, to: Text) -> Text` | Every `from` in `t` replaced by `to`. *Fails* for an empty `from`. |
| `slice(t: Text, start: Int, end: Int) -> Text` | The characters from `start` up to, not including, `end`. *Fails* unless `0 <= start <= end <= len(t)`. |
| `index_of(t: Text, part: Text) -> Optional<Int>` | The position of the first `part`, or `none`. `some(0)` for an empty `part`. |
| `split(t: Text, sep: Text, N) -> List<Text, N>` | The parts of `t` between each `sep`. `N` is an integer literal, at least 1. *Fails* for an empty `sep` or more than `N` parts. |
| `join(xs: List<Text, N>, sep: Text) -> Text` | The texts with `sep` between them. *Fails* if the result exceeds 16 MiB. |
| `to_int(t: Text) -> Optional<Int>` | The integer `t` writes in decimal (an optional `-` and digits, nothing else, in range), or `none`. |
| `to_text(x) -> Text` | `x` (`Text`, `Int`, `Bool` or `Money`) as text: `12.50 USD` for money. |
| `json(x) -> Text` | `x` as JSON. Records are objects, money is `{"amount", "currency"}`, `none` is `null`. Secrets, headers and capabilities cannot be encoded. |
| `url_segment(t: Text) -> Text` | `t` percent-encoded as one URL path segment. *Fails* for `""`, `"."` and `".."`. |
| `form(r) -> Text` | A record of `Text`, `Int` and `Bool` fields, form-encoded (`a=x%20y&n=2`). |

## Numbers and money

| Function | Result |
| --- | --- |
| `abs(n: Int) -> Int` | *Fails* for the most negative `Int`. |
| `min(a, b)`, `max(a, b)` | Of two `Int` or two `Money` values. *Fails* for money in different currencies. |
| `rem(a: Int, b: Int) -> Int` | The remainder of `a / b`, with the sign of `a`. *Fails* when `b` is 0. |
| `minor_units(m: Money) -> Int` | The amount in minor units: cents for USD. |
| `currency(m: Money) -> Text` | The currency code, such as `"USD"`. |

## Lists

No list function changes its argument; each returns a new list.

| Function | Result |
| --- | --- |
| `len(xs) -> Int` | The number of items. |
| `contains(xs, x) -> Bool` | Whether `x` is an item. |
| `append(xs: List<T, N>, x: T) -> List<T, N>` | `x` added at the end. *Fails* if `xs` already holds `N` items. |
| `concat(a: List<T, N>, b: List<T, M>) -> List<T, N+M>` | `a`'s items, then `b`'s. |
| `reverse(xs) -> List<T, N>` | |
| `slice(xs, start: Int, end: Int)`, `index_of(xs, x)` | As for text. |
| `sort(xs) -> List<T, N>` | Ascending, for `Int`, `Text` or `Money`; text by its bytes; equal items keep their order. *Fails* for money in different currencies. |
| `sum(xs: List<Int, N>) -> Int` | 0 for an empty list. *Fails* on overflow. |
| `map(xs: List<T, N>, f: fn(T) -> U) -> List<U, N>` | `f` of each item. |
| `filter(xs, keep: fn(T) -> Bool) -> List<T, N>` | The items for which `keep` is true. |
| `count(xs, test: fn(T) -> Bool) -> Int` | How many items `test` is true for. |
| `any(xs, test)`, `all(xs, test) -> Bool` | Whether one, or every, item passes. They stop at the first item that decides. |
| `fold(xs: List<T, N>, start: A, f: fn(A, T) -> A) -> A` | `start` passed through `f` with each item in turn. |

## Maps and sets

| Function | Result |
| --- | --- |
| `get(m: Map<K, V, N>, k: K) -> Optional<V>` | The value under `k`, or `none`. |
| `put(m, k, v) -> Map<K, V, N>` | `m` with the entry added or replaced. *Fails* if a new key would exceed `N`. |
| `remove(m, k)`, `remove(s, x)` | Without that key or item. |
| `keys(m) -> List<K, N>`, `values(m) -> List<V, N>` | In key order. |
| `entries(m) -> List<(K, V), N>` | The entries as tuples, in key order. |
| `add(s: Set<T, N>, x: T) -> Set<T, N>` | `s` with `x`. *Fails* if a new item would exceed `N`. |
| `items(s) -> List<T, N>` | In order. |
| `to_set(xs: List<T, N>) -> Set<T, N>` | The distinct items of a list. |
| `len`, `contains` | Take maps (by key) and sets. |

## Hashing and encoding

These work on the UTF-8 bytes of a text.

| Function | Result |
| --- | --- |
| `sha256(t: Text) -> Text` | The SHA-256 digest as 64 lowercase hexadecimal digits. |
| `base64_encode(t: Text) -> Text` | Base64 with the standard alphabet and padding (RFC 4648, section 4). |
| `base64_decode(t: Text) -> Optional<Text>` | The text `t` encodes, or `none` if `t` is not exactly that form or does not decode to UTF-8. |
| `hex_encode(t: Text) -> Text` | Two lowercase digits per byte. |
| `hex_decode(t: Text) -> Optional<Text>` | Digits in either case; `none` for an odd count, other characters, or bytes that are not UTF-8. |

## Dates and times

A time is an `Int` of milliseconds since 1970-01-01T00:00:00Z, in UTC, from the year
0000 to 9999, without leap seconds. A duration is an `Int` of milliseconds.

| Function | Result |
| --- | --- |
| `format_time(t: Int) -> Text` | `YYYY-MM-DDTHH:MM:SS.mmmZ`. |
| `parse_time(t: Text) -> Optional<Int>` | An RFC 3339 time: `YYYY-MM-DDTHH:MM:SS`, optionally `.` and 1 to 3 digits, then `Z` or `+HH:MM` or `-HH:MM`; `none` for anything else. |
| `make_time(year, month, day, hour, minute, second) -> Int` | *Fails* for a date or time that does not exist. |
| `date_parts(t: Int) -> (Int, Int, Int)` | Year, month, day. |
| `time_parts(t: Int) -> (Int, Int, Int, Int)` | Hour, minute, second, millisecond. |
| `weekday(t: Int) -> Int` | 1 for Monday to 7 for Sunday. |

The time functions *fail* for a time outside the range.

## Input, output, time and randomness

| Function | Result |
| --- | --- |
| `print(t: Text)` | Writes `t` and a line break to standard output. |
| `now() -> Int` | The current time, from the system clock. |
| `random_int(lo: Int, hi: Int) -> Int` | A uniformly chosen integer from `lo` to `hi` inclusive. *Fails* if `lo > hi`. |
| `read_line() -> Optional<Text>` | The next line of standard input without its line break, or `none` at the end of the input. Its result is untrusted. |

These differ between runs, so each is recorded in the journal: a resumed run uses the
values the first run recorded, and does not print a recorded line again. They cannot
be used in a constant, a default value or a function value.

## HTTP requests and secrets

| Function | Result |
| --- | --- |
| `header(name: Text, value: Text or Secret) -> Header` | A request header. A header that holds a secret follows the rules for secrets. |
| `fill_secret(template: Text, key: Secret) -> Secret` | The template with `{secret}` replaced by the key, as a new secret, such as `"Bearer {secret}"`. *Fails* unless the template contains `{secret}` exactly once. |
| `parse(T, t: Text) -> Proposed<T>` | JSON text from outside, unchecked until `validate`. Fields the type does not declare are dropped when validated. |

## Examples

```staal file=std.staal
fn text_examples() -> Text {
    let parts = split("a,,b", ",", 5)
    let joined = join(reverse(parts), "|")
    let at = index_of("hello", "l")
    let n = to_int("-42")
    return json([upper("straße"), slice("hello", 1, 4), joined, json(at), json(n), sha256("abc")])
}

fn list_examples() -> Text {
    let xs: List<Int, 10> = [3, 1, 2]
    let doubled = map(xs, fn(x: Int) -> Int { return x * 2 })
    let big = filter(xs, fn(x: Int) -> Bool { return x > 1 })
    let total = fold(xs, 0, fn(a: Int, x: Int) -> Int { return a + x })
    return json([sort(xs), append(xs, 4), doubled, big]) + " " + json(total) + " " + json(rem(-7, 2))
}

fn time_examples() -> Text {
    let t = make_time(2026, 11, 15, 9, 30, 0)
    return format_time(t + 86400000) + " " + json(weekday(t)) + " " + json(date_parts(t))
}
```

```console
$ staalc run std.staal --entry text_examples
"[\"STRASSE\",\"ell\",\"b||a\",\"2\",\"-42\",\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"]"
$ staalc run std.staal --entry list_examples
"[[1,2,3],[3,1,2,4],[6,2,4],[3,2]] 6 -1"
$ staalc run std.staal --entry time_examples
"2026-11-16T09:30:00.000Z 7 [2026,11,15]"
```
