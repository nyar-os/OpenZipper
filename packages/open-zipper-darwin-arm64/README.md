# open-zipper-darwin-arm64

Platform package for the `open-zipper` native addon on macOS arm64.

The `*.node` binary is produced by `scripts/build-napi.ts` and copied here by
`scripts/stage-native.ts`. It is not committed. Build before publishing:

```bash
pnpm run build:napi
```

The app package lists this package under `optionalDependencies`, so npm and
pnpm install it only on matching `os` / `cpu` targets.
