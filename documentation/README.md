# OpenZipper 开发者手册

## Layout

Rust in `crates/` and TypeScript in `packages/`. Native capability exports live in `crates/napi`; the CLI, GUI and skills only consume them.

## Development

```sh
pnpm install
pnpm run build        # native addon, TypeScript packages, GUI
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Contributions must keep the in-process boundary: no external archive binaries, no shell pipelines that move work to another process.

## Archive safety

Zip Slip, absolute paths, drive letters, UNC names, link escapes, Windows trailing dot/space and reserved device names are rejected before writing. Bomb guards: entry count, per-entry size, total size, compression ratio and nested archive depth.

Encoding: UTF-8 first, then GBK, then a CP437 table.

Streaming: zip creation compresses straight into the output file and zip / tar.gz extraction decodes member by member with limits enforced during the byte flow, so peak memory follows one 64 KiB chunk instead of the archive size. tar.bz2 and tar.xz still decode into memory; plain tar creation builds the tar image before the final rename.

## 7z coverage

LZMA, LZMA2, PPMd, BZip2 and Deflate plus the BCJ and Delta filters. Encrypted archives and unsupported codecs fail with `encrypted_archive` and `unsupported_compression`. `rar` remains `dependency_blocked`: the UnRAR source is custom freeware, not Apache-2.0 or MIT.

## Platform notes

Archive handling is plain filesystem work, so the core capabilities are available on all three desktop platforms. NTFS alternate data streams and ACL preservation are `not_implemented` on Windows and `unsupported_platform` elsewhere. Every unavailable capability carries one of three stable codes: `unsupported_platform`, `not_implemented`, `dependency_blocked`.

## Audit

Every capability call is appended to a local audit trail with a redacted payload through the shared `wae.audit/v0` schema.
