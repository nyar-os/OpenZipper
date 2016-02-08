# OpenZipper

Open, inspect, extract, create and verify archives — zip and tar (with gzip, bzip2 and xz wrappers) — on Windows, macOS and Linux.

**All archive work runs inside the process.** OpenZipper never shells out to `7z`, `tar`, `unzip`, `zip`, `bsdtar`, `rar` or any other external tool. Extraction, creation, entry listing and integrity checks are implemented against in-tree Rust libraries, so behaviour is identical on every host and nothing depends on what a user happens to have installed.

## What it does

| Area | Detail |
|------|--------|
| Formats | `zip`, `tar`, `tar.gz` (`.tgz`), `tar.bz2` (`.tbz2`), `tar.xz` (`.txz`) |
| Safety | Zip Slip, absolute paths, drive letters, UNC names, link escapes, Windows trailing dot/space and reserved device names are rejected before writing |
| Bomb guards | Entry count, per-entry size, total size, compression ratio and nested archive depth, all with explicit errors |
| Encoding | UTF-8 first, then GBK, then a CP437 table, so mixed zip name encodings read back correctly |
| Streaming | Entries are streamed while writing; archives are never loaded whole into memory |
| Collisions | `error` (default), `skip`, or `rename` — never a silent overwrite |
| Audit | Every capability call is appended to a local audit trail with a redacted payload |

`7z` and `rar` report `dependency_blocked` with a stable capability code instead of degrading to an external binary or a partial parser.

## Install

```sh
npm install -g @wae/open-zipper
```

The CLI and GUI share one native addon. Platform packages exist for Windows x64, macOS x64/arm64 and Linux x64/arm64.

## CLI

```sh
# Inspect before extracting
open-zipper list archive.zip --json

# Preview what would be written
open-zipper extract archive.zip --dest ./out --dry-run

# Extract, with an explicit collision policy
open-zipper extract archive.zip --dest ./out --overwrite rename

# Verify every entry can be read back
open-zipper test archive.tar.gz

# Package files
open-zipper create ./photos --out photos.zip --format zip

# What this build supports
open-zipper capabilities --json

# What ran on this machine
open-zipper audit --json --limit 20
```

Every command accepts `--json`. Exit codes are stable: `0` success, `1` runtime failure, `2` usage, `3` capability unavailable.

## Library

```ts
import { extract, listEntries, createArchive } from '@wae/open-zipper';

const listing = listEntries({ path: 'download.zip' });
if (listing.unsafeEntryCount > 0) throw new Error('unsafe archive');

const report = extract({ path: 'download.zip', destination: './out', overwrite: 'rename' });
console.log(report.written, report.bytesWritten);
```

## GUI

The desktop GUI is a static VMZ application served by a WebView host. It shows archive entries with safety annotations and reports the extraction outcome.

```sh
open-zipper gui
```

## Platform notes

Archive handling is plain filesystem work, so the core capabilities are available on all three desktop platforms. Only host-specific metadata handling is gated:

- NTFS alternate data streams and ACL preservation are `not_implemented` on Windows and `unsupported_platform` elsewhere.
- Every unavailable capability carries one of three stable codes: `unsupported_platform`, `not_implemented`, `dependency_blocked`.

## Development

```sh
pnpm install
pnpm run build        # native addon, TypeScript packages, GUI
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace keeps Rust in `crates/` and TypeScript in `packages/`. Native capability exports live in `crates/napi`; the CLI, GUI and skills only consume them. Contributions must keep the in-process boundary: no external archive binaries, no shell pipelines that move work to another process.

## License

Apache-2.0.
