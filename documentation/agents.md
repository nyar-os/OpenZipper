# OpenZipper Agent 参考

## CLI 命令

```sh
open-zipper list archive.zip --json
open-zipper extract archive.zip --dest ./out --dry-run
open-zipper extract archive.zip --dest ./out --overwrite rename
open-zipper test archive.tar.gz
open-zipper create ./photos --out photos.zip --format zip
open-zipper capabilities --json
open-zipper audit --json --limit 20
open-zipper gui
```

Every command accepts `--json`.

## Exit codes

- `0` — success
- `1` — runtime failure
- `2` — usage
- `3` — capability unavailable

## 支持的格式

Read: `zip`, `7z`, `tar`, `tar.gz` (`.tgz`), `tar.bz2` (`.tbz2`), `tar.xz` (`.txz`). Write: `zip` and the tar family.

## Collision policy

`error` (default) — error out on duplicate path
`skip` — keep existing, skip incoming
`rename` — incoming gets a suffix, never a silent overwrite

## Platform notes

Only host-specific metadata handling is gated. Every unavailable capability carries one of three stable codes: `unsupported_platform`, `not_implemented`, `dependency_blocked`.
