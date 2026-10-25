# main.staal

Sends one note.

The program's own documentation.

## Types

### `Body`

```staal
type Body = Text
```

## Constants

### `LIMIT`

```staal
const LIMIT = 280
```

Most characters in a body.

## Functions

### `fits`

```staal
fn fits(
    n: lib.Note,
    most: Int,
) -> Bool
```

A note that fits.

### `main`

```staal
fn main(to: Text, body: Body, sender: cap notes.send.deliver) -> Receipt
```

# Module ./lib.staal

Notes: a small library.

## Types

### `Note`

```staal
pub type Note { to: Text, body: Text }
```

A note to send.

### `Priority`

```staal
pub enum Priority {
    Low,
    High { why: Text },
}
```

## Functions

### `size`

```staal
pub fn size(n: Note) -> Int
```

The length of a note's body.

## Capabilities

### `notes.send`

```staal
capability notes.send {
    needs post: cap net.http.post
    needs base: Text
    canonical(to: Text) -> Text
    action deliver(to: Text @target, body: Text) -> Text
}
```

Sends notes through a web service.
