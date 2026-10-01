# Agent skills

[Agent Skills](https://code.claude.com/docs/en/skills) for driving `landrop-cli` from an agent tool. The format is the shared `SKILL.md` convention (`name` + `description` frontmatter plus a markdown body), so one skill serves several tools.

```
skills/
  landrop-cli/
    SKILL.md                      the skill: workflow, human boundary, limits
    references/
      commands.md                 every command, flag and JSON field
      troubleshooting.md          error text -> cause -> next action
```

## Install

A skill is discovered from a *skills root*. Copy (or symlink) the `landrop-cli` directory into one of them:

| Tool | Project root | User root |
|---|---|---|
| DeepSeek Harness | `<repo>/.agents/skills` or `<repo>/.dsh/skills` | `~/.agents/skills` or `~/.dsh/skills` |
| Claude Code | `<repo>/.claude/skills` | `~/.claude/skills` |
| opencode | `<repo>/.opencode/skills` | `~/.config/opencode/skills` |

```console
# DeepSeek Harness, for the current user
mkdir -p ~/.agents/skills
cp -r skills/landrop-cli ~/.agents/skills/

# Claude Code, for the current user
mkdir -p ~/.claude/skills
cp -r skills/landrop-cli ~/.claude/skills/
```

Windows, using the same layout:

```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\.agents\skills" | Out-Null
Copy-Item -Recurse -Force skills\landrop-cli "$env:USERPROFILE\.agents\skills\"
```

## Two things that will waste your time otherwise

1. **The skill directory must sit directly under the skills root.** Nested `**/SKILL.md` files are deliberately not discovered — so pointing a root at this repository does *not* find `skills/landrop-cli/SKILL.md`. Copy the `landrop-cli` directory itself, as above.
2. **The CLI must be on `PATH`.** The skill shells out to `landrop-cli`. Build it with `cargo build --release`, then either put `target/release` on `PATH` or install the binary somewhere already on it.

## Verify

```console
$ landrop-cli selftest --json     # the binary works, no peer needed
$ landrop-cli discover --json     # the network side works
```

Then ask the agent something that should load the skill, for example "send ./report.pdf to the build server". If it does not reach for the skill, the `description` in the frontmatter is what needs work — that text is what the agent matches against.

## If the agent does not see the skill

Work through these in order:

1. **Is the skill subsystem enabled in that agent tool?** DeepSeek Harness ships the pieces but does not enable them all by default — check that both `@deepseek-ai/dsh-skill-filesystem` (which discovers skills on disk) and `@deepseek-ai/dsh-tool-skill` (which exposes the loader) are enabled. With only the registry `@deepseek-ai/dsh-skill` active, the catalog is empty and every lookup fails with "unknown or no longer available".
2. **Is the directory directly under a root?** See the nesting note above.
3. **Is `landrop-cli` on `PATH`?** The skill shells out to it.
4. **Is the description specific enough?** The agent decides whether to load a skill from the frontmatter `description` alone. If the agent never reaches for it, that text is what to improve, not the body.

## Adding a skill for agent-to-agent messaging

There is no skill here for messaging between agents: the CLI cannot do it yet. Writing a skill for an unimplemented capability would only teach agents to invent one.
