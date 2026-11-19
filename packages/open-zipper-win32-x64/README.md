# open-zipper-win32-x64

Platform package for the `open-zipper` native addon on Windows x64.

The `*.node` binary is produced by `wae native build` and copied here by
`wae native stage`. It is not committed. Build before publishing:

```bash
pnpm run build:napi
```

The app package lists this package under `optionalDependencies`, so npm and
pnpm install it only on matching `os` / `cpu` targets.
