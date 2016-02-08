# OpenZipper skills

Agent-facing skills for the `open-zipper` CLI.

| Skill | Use when |
|-------|----------|
| `open-zipper` | Inspecting, extracting, creating or verifying zip and tar archives |

Install the owning package to get the CLI:

```sh
npm install -g @wae/open-zipper
```

Every skill is a `SKILL.md` folder that an agent can load. Archive work runs in-process, with path-safety and bomb checks enabled by default.
