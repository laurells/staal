# Packages

A package is a git repository of Staal modules with a `staal.json` that names it and
declares its authority: every capability its code may use, and the targets it may
reach. A project names its dependencies in its own `staal.json`; `staalc fetch` reads
them from git, verifies them, and records them in `staal.lock`.

## Depending on a package

`staal.json`, next to the program:

```json
{
  "dependencies": {
    "sms": { "git": "https://github.com/laurells/staal-sms", "commit": "1bd0e7645a505227084bc83379affbcd71732560" },
    "email": { "git": "https://github.com/laurells/staal-email", "version": "^1.2" }
  }
}
```

| Form | Resolves to |
| --- | --- |
| `git` and `commit` | Exactly that commit (40 lowercase hexadecimal digits). |
| `git` and `version` | The newest release of the repository within the range. A release is a tag `v1.2.0` or `1.2.0` on a commit whose `staal.json` gives that version. |
| `version` | The newest version within the range in the project's `registry`. |

A location is `https://`, `ssh://`, `user@host:path`, `file://` or an absolute path.
Plain `http://`, `git://`, branch names, and a tag given as a commit are refused.

Versions follow Semantic Versioning 2.0.0. A range is comparators separated by commas:
`1.2.3` or `^1.2.3` (at least 1.2.3, below 2.0.0; for `0.x`, below the next minor
version), `~1.2.3` (below 1.3.0), `=1.2.3`, `>=1.2`, `>1.2`, `<2`, `<=1.4`, `1.*`, `*`. A
pre-release is chosen only by a range that names a pre-release of the same version.

```sh
staalc fetch
```

`fetch` chooses one version of each package that fits every range asking for it (the
newest, or the one already locked while it still fits), reads each package from git
into `.staal/packages/`, and writes `staal.lock` with each package's commit, version and
the SHA-256 of every file. It prints each package's authority, for review:

```text
sms https://github.com/laurells/staal-sms at 1bd0e7645a50 (new)
    authority: net.http.post only https://api.ng.termii.com/api/sms/send
```

When no version fits every range, it stops and says which packages ask for which
versions. A release tag or registry entry that moved stops the fetch with nothing
changed. `staalc update` moves locked versions to the newest that fit; `staalc update
--package <name>` moves one. `git` must be installed.

Commit `staal.json` and `staal.lock`. `.staal/` can be fetched again.

## Using a package

A package's modules are imported by the package's name:

```text
import sms.termii

fn remind(phone: Text, texts: cap sms.termii.send(uses: 1)) -> Receipt {
    return texts.send(phone, "Your appointment is tomorrow.")
}
```

The program can import only packages it depends on itself, and one version of each
package is used across the project. Inside a package, `import util` means the package's
own `util`, never the program's.

## Verification

Before every check and run, each package folder is compared with `staal.lock`: a file
that was changed, added or removed, or replaced by a link, is refused.

```text
staalc: package greet does not match staal.lock: words.staal has been changed
```

`staalc fetch` reads files from git's objects, not from a checkout, and compares them
with the lock each time, so an edited lock is also caught. A journal records every
package file, so a run cannot be resumed after a package changed.

## Writing a package

The package's `staal.json`:

```json
{
  "package": "notes",
  "version": "0.1.0",
  "authority": {
    "net.http.post": ["https://api.example.com/v1/notes"]
  }
}
```

| Key | Contents |
| --- | --- |
| `package` | The name projects depend on it by: lowercase letters, digits and `_`, starting with a letter. `net`, `fs`, `db` and `ffi` are refused. |
| `version` | Its version. Required for a package fetched by version range. |
| `authority` | Required. Each key is a capability path; the value is `"any"`, or a list of `only` patterns. `{}` for a package that uses no capability. |
| `dependencies` | Its own dependencies, as in a project. A package cannot name a registry. |

The compiler holds a package to its manifest (E108): its code may use only the
capabilities `authority` lists, and where `authority` lists targets, every such
capability in its code must carry an `only` within them. A package's own capabilities
must be named after it (`notes.*`). A reviewer can read `authority` to know what the
package can reach.

To release a version, commit with that `version` in `staal.json` and tag the commit
`v<version>`. Projects then depend on it by `git` and `version`.

[PACKAGES.md](../PACKAGES.md) builds a package for a real service step by step,
including tests against a local stand-in for the service.

## Registries

A registry is a git repository that lists each published version of each package, with
its commit and the digest of its files. A project names one with `"registry"` in its
`staal.json` and depends on packages by `version` alone.

```sh
staalc publish --registry ./registry --git https://github.com/you/notes --commit <id>
staalc yank --registry ./registry --package notes --version 0.1.0
```

`publish` adds a version to a local clone of the registry as a commit, refusing one
already published; pushing the registry publishes it. `yank` marks a version not to be
chosen again; projects that locked it keep it. There is no public registry. Whoever can
push to a registry can publish under any name, so a shared registry needs its changes
reviewed.

## Reference packages

[packages](../../packages/README.md) holds packages for Termii, Stripe, Paystack,
Flutterwave, Resend, Postmark, SendGrid, Slack, Discord, GitHub and Twilio, and a
template to start from. `sms`, `payments`, `email` and `chat` are also published as
their own repositories (`laurells/staal-sms`, `staal-payments`, `staal-email`,
`staal-chat`).
