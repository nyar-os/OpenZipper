---
name: open-zipper
description: Inspect, extract, create and verify zip and tar archives from an agent workflow using the open-zipper CLI. Use when a task involves unpacking a downloaded archive, checking an archive before it is extracted, packaging files for delivery, or diagnosing a corrupt archive. All archive work happens in-process, with path-safety and bomb checks enabled by default.
---

# OpenZipper

`open-zipper` is a command-line archive tool with a native core. Listing, extraction, creation and integrity checks run inside the process — no external `7z`, `tar`, `unzip` or `zip` binary is invoked, so the same behaviour applies on every supported host.

Supported formats: `zip`, `tar`, `tar-gz`, `tar-bz2`, `tar-xz`. `7z` and `rar` report `dependency_blocked` with a stable capability code instead of silently degrading.

## Workflow

1. Inspect before extracting. Listing is read-only and annotates unsafe entries:

    `open-zipper list <archive> --json`

2. Preview an extraction. A dry run validates paths and limits without touching the filesystem:

    `open-zipper extract <archive> --dest <dir> --dry-run --json`

3. Extract with an explicit collision policy. The default is `error`, never a silent overwrite:

    `open-zipper extract <archive> --dest <dir> --overwrite <policy> --json`

4. Verify an archive before publishing it:

    `open-zipper test <archive> --json`

5. Package files for delivery. Existing output is refused unless `--force` is passed:

    `open-zipper create <source> --out <archive> --format <name> --json`

6. Check what the host build supports:

    `open-zipper capabilities --json`

7. Read the capability history when you need to prove what ran:

    `open-zipper audit --json --limit <n>`

## Exit codes

- `0` — success
- `1` — runtime failure (corrupt archive, I/O error, refused overwrite)
- `2` — usage error (missing or invalid argument)
- `3` — capability unavailable (`unsupported_platform`, `not_implemented`, `dependency_blocked`)

## Safety

- Treat every archive from an untrusted source as hostile. Run `list` and `extract --dry-run` first when the contents are unknown.
- Path checks reject parent traversal, absolute paths, drive letters, UNC names, symlink and hardlink escapes, and Windows trailing-dot/space or reserved-device-name tricks before anything is written.
- Bomb guards cap entry count, per-entry size, total size, compression ratio and nested archive depth. Do not raise the limits to make a suspicious archive pass.
- Extraction never replaces existing files unless the caller chose `--overwrite skip` or `--overwrite rename`, or explicitly asked for `error` to fail.
- Do not extract into a directory that contains unrelated data, and prefer a fresh directory per archive.
- Audit records contain redacted parameters only. Do not put secrets in archive names or paths.
