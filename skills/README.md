# Agent skills

`slcr/` is a [Claude skill](https://docs.claude.com/en/docs/agents-and-tools/agent-skills/overview)
for repositories that use SLCR. It teaches an agent why and when to use SLCR,
how to learn the `slcr` CLI one level at a time with `--help`, and how to write
specifications and validators.

To add it to your repository, run this from the repository root:

```bash
mkdir -p .claude/skills
curl -fsSL https://github.com/wack/slcr/archive/refs/heads/main.tar.gz \
  | tar -xz --strip-components=2 -C .claude/skills slcr-main/skills/slcr
```

Then commit `.claude/skills/slcr/`. Claude Code loads it automatically, and
uses it whenever the repository has an `SLCR.toml`, a spec, or validator
files, or whenever a task mentions requirements. Update it by running the
same command again.

The skill describes concepts and workflows. It defers to `slcr --help` for
commands and flags, so it stays correct across CLI releases.
