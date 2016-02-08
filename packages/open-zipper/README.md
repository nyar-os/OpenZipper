# @wae/open-zipper

List, extract, create and verify zip and tar archives in-process.

```sh
npm install -g @wae/open-zipper
open-zipper list archive.zip --json
open-zipper extract archive.zip --dest ./out --dry-run
open-zipper extract archive.zip --dest ./out --overwrite rename
open-zipper create ./photos --out photos.zip
open-zipper capabilities --json
```

All archive work — listing, extraction, creation, integrity checks — runs inside the native core. No external `7z`, `tar`, `unzip` or `zip` binary is invoked.

Every command accepts `--json`. Exit codes: `0` success, `1` runtime failure, `2` usage, `3` capability unavailable.

See the workspace README for the full safety model, supported formats and platform notes.
