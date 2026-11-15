# Files, databases and sockets

The runtime performs these capabilities itself, with no adapter, and checks and journals
them like any other action. HTTP requests are covered in
[Capabilities](capabilities.md#the-runtimes-own-capabilities). The full rules are in
[RUNTIME.md](../RUNTIME.md), section 12.

| Capability | Action | Result |
| --- | --- | --- |
| `fs.read` | `c.read(path)` | `Text` |
| `fs.write` | `c.write(path, content)` | `Receipt` |
| `db.sqlite.query`, `db.postgres.query` | `c.query(database, sql)`, `c.query(database, sql, params)` | `Text` (JSON rows) |
| `db.sqlite.execute`, `db.postgres.execute` | `c.execute(database, sql)`, `c.execute(database, sql, params)` | `Receipt` |
| `net.tcp.exchange`, `net.tls.exchange` | `c.exchange(address, data)`, `c.exchange(address, data, until)` | `Text` |
| `net.tcp.accept`, `net.tls.accept` | `c.accept(address)`, `c.accept(address, until)` | `Incoming` |
| `net.tcp.reply`, `net.tls.reply` | `c.reply(connection, data)` | `Receipt` |

All arguments are `Text`, except `params`, a `List<Text, N>`. `uses` and `only` apply;
`each` and `max` do not, since no money is spent.

## Files

A path must be absolute. It is made canonical before `only` is checked: empty and `.`
segments are removed and `..` is resolved as text.

```staal file=report.staal
fn save(path: Text, text: Text, out: cap fs.write(only: ["/srv/reports/*"], uses: 1)) -> Receipt {
    return out.write(path, text)
}
```

```json file=report.json
{ "entry": "save", "inputs": { "path": "/srv/reports/../../etc/cron.d/job", "text": "x" }, "grants": { "out": { "cap": "fs.write" } } }
```

```console
$ staalc run report.staal --config report.json
staalc: run failed: capability: 'fs.write' may only act on ["/srv/reports/*"]; this action targets "/etc/cron.d/job"
```

- A path is opened one directory at a time, and no symbolic link is followed.
- A read accepts only a regular file with one hard link, stops at `fs.max_read_bytes`
  (16 MiB by default), and requires UTF-8.
- A write goes to a temporary file in the same directory and replaces the target with
  one rename, then the directory is flushed: a crash never leaves half a file. The
  receipt's id is derived from the content's SHA-256.
- Reads and writes are safe to repeat after a crash.

Linux and macOS only; elsewhere every file action fails.

## SQLite

The database is a file path, checked against `only` and opened as `fs` opens a path.

```staal file=orders.staal
type Order { id: Int, total: Text }

fn open_orders(customer: Text, db: cap db.sqlite.query(only: ["/var/lib/shop/shop.db"])) -> List<Order, 100> {
    let rows = db.query("/var/lib/shop/shop.db", "select id, total from orders where customer = ?1 and status = 'open'", [customer])
    let orders: List<Order, 100> = validate parse(List<Order, 100>, rows)
    return orders
}

fn close_order(id: Text, db: cap db.sqlite.execute(only: ["/var/lib/shop/shop.db"], uses: 1)) -> Receipt {
    return db.execute("/var/lib/shop/shop.db", "update orders set status = 'closed' where id = ?1", [id])
}
```

- `query` opens the database read-only and runs one statement. It returns the rows as
  a JSON array of objects by column name: INTEGER as a number, TEXT as a string, NULL as
  null, REAL as the shortest decimal text that reads back as the same number. A BLOB,
  two columns of one name, more than 10,000 rows, or more than `fs.max_read_bytes` of
  JSON fails (`action`). The database must exist.
- `execute` runs one statement in a transaction with `synchronous=FULL`; it creates the
  database if it does not exist. The receipt is `sqlite:N`, N the rows changed.
- The SQL must be the program's own text (E006). Values from outside are `params`,
  bound to `?1`, `?2`, and so on. Parameters are text, converted by each column's
  affinity; their number must match the statement's.
- One statement per action. `ATTACH` is refused.
- A statement running longer than `net.timeout_ms` is stopped and changes nothing.
  Waiting for another process's lock is limited to 5 seconds.

Linux only; elsewhere every database action fails.

## PostgreSQL

The grant names the databases a capability reaches; the program's `database` argument
is one of those names. `only` does not apply.

```staal file=customers.staal
type Customer { id: Int, name: Text }

fn find(email: Text, db: cap db.postgres.query) -> List<Customer, 10> {
    let rows = db.query("shop", "select id, name from customers where email = $1", [email])
    let found: List<Customer, 10> = validate parse(List<Customer, 10>, rows)
    return found
}
```

```json
{
  "grants": {
    "db": { "cap": "db.postgres.query",
            "databases": { "shop": { "host": "db.internal", "dbname": "shop", "user": "reader",
                                     "password": { "env": "SHOP_DB_PASSWORD" } } } }
  }
}
```

| Key | Contents |
| --- | --- |
| `host`, `dbname`, `user` | Required. |
| `port` | Default 5432. |
| `password` | `{ "env": "NAME" }`, a secret from the environment. Omit it for a server that asks for none. |
| `tls` | `"disable"` is allowed only for `localhost`, `127.0.0.1` and `::1`. Otherwise every connection uses TLS, verified against Mozilla's roots, `net.extra_root_certificates`, and `host`. |

A name the grant does not list is refused:

```text
staalc: run failed: capability: the grant reaches no database named 'billing' (it reaches shop)
```

- `query` runs in a READ ONLY transaction. Integer types and `oid` are numbers;
  `boolean` is true or false; text types and enums are strings; `numeric` is its exact
  decimal text; `date`, `timestamp` and `timestamptz` (in UTC, with `Z`) are ISO 8601
  text; `uuid` is its usual form; `json` and `jsonb` are their text. Any other type
  fails, with a message saying how to cast it.
- `execute` runs one statement with `synchronous_commit` on. The receipt is
  `postgres:N`.
- Parameters are `$1`, `$2`, and so on, sent apart from the SQL as text.
- Each action opens its own connection. The connect limit is
  `net.connect_timeout_ms`; a statement past `net.timeout_ms` is cancelled by the
  server.

## Statements and crashes

A query is recorded in the journal with its rows; a resumed run gets them again without
opening the database. A statement is recorded by hashes of its SQL and parameters. If
a crash or a lost connection leaves it in doubt, the run stops for an operator
(`staalc resolve`), unless the grant says the statement is keyed:

```json
{ "grants": { "db": { "cap": "db.sqlite.execute", "idempotency_key": true } } }
```

Each statement's key is then written, in the same transaction, to a table `_staal_keys`
in the database. A statement in doubt happened exactly when its key is there, and the
resumed run settles it without an operator. See [Journals and resuming](durability.md).

## TCP and TLS exchanges

`exchange` connects to `host:port`, sends `data`, and returns the reply. Without
`until`, it closes its sending side after the data and reads until the peer closes.
With `until`, such as `"\r\n"`, it reads until the reply ends with it.

```staal file=ping.staal
fn ask(line: cap net.tcp.exchange(only: ["127.0.0.1:7000"], uses: 1)) -> Text {
    return line.exchange("127.0.0.1:7000", "PING\r\n", "\r\n")
}
```

```json file=ping.json
{ "entry": "ask", "grants": { "line": { "cap": "net.tcp.exchange" } } }
```

```console
$ staalc run ping.staal --config ping.json
staalc: run failed: action: not sent: 127.0.0.1:7000: 127.0.0.1 is not a public address, and is not in this grant's private_addresses
```

The grant needs `"private_addresses": ["127.0.0.1"]` to reach it.

- An `only` pattern is an address, `host:*` (any port of one host), or `*`.
- `net.tcp.exchange` is not encrypted. `net.tls.exchange` verifies the certificate
  against Mozilla's roots and `net.extra_root_certificates`, and against the host.
- The whole exchange is limited by `net.timeout_ms`, the connection by
  `net.connect_timeout_ms`. A reply longer than `net.max_response_bytes` or not UTF-8
  fails (`action`), although the exchange happened.
- A failure before anything was sent begins `not sent:`. Once data may have reached
  the peer, a broken connection or a timeout leaves the exchange in doubt. It is never
  repeated: the run stops until an operator records the reply with `staalc resolve
  --reply <text>`, or `--failed` or `--retry`.

## Listening

`accept` listens on an address and returns the next client's request as
`Incoming { connection: Text, data: Text }`. `reply` sends data on that connection and
closes it.

```staal file=serve.staal
fn serve_one(inbox: cap net.tcp.accept(only: ["127.0.0.1:8080"], uses: 1), answer: cap net.tcp.reply(uses: 1)) -> Receipt {
    let req = inbox.accept("127.0.0.1:8080", "\n")
    return answer.reply(req.connection, "received " + json(len(req.data)) + " characters\n")
}
```

- A run starts listening at its first `accept` on an address and stops when it ends.
  Every `accept` on one address, in any task, takes clients from one queue, so tasks
  made by `parallel` serve clients at once.
- `127.0.0.1:8080` takes connections from the same machine only; `0.0.0.0:8080` from
  any.
- `reply` takes no `only`: it answers only a connection the run accepted.
- The request is untrusted (E006). An `Incoming` cannot be built in code, proposed by a
  model or parsed from text.

Accept grant keys:

| Key | Contents |
| --- | --- |
| `wait_ms` | 1 to 2147483647. The longest an `accept` waits for a client; past it, it fails with `no client connected to <address> within <n> ms`. Without it, an `accept` waits indefinitely. Required for a tool served by `staalc mcp`. |
| `on_bad_client` | `"skip"` (default): a bad client (a failed TLS handshake, or a request that is too slow, too large or not UTF-8) is dropped, logged as `client_dropped`, and the next client is taken. `"fail"`: the `accept` fails. |
| `certificate_chain` | `net.tls.accept` only. A PEM file of the server's certificate and intermediates, with no private key. |
| `private_key` | `net.tls.accept` only. A PEM file of its key, owned by the user running `staalc`, mode 600 or 400. |

The certificate and key are read when the run starts; a certificate that is not valid
at that moment, or a key that does not match, is a config error. Client certificates
are not requested.

The journal records each request and a hash of each reply. A connection does not
outlive the process: when a run stops between `accept` and `reply`, the resumed run's
`reply` fails and says so.
