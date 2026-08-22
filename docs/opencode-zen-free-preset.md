# OpenCode Zen Free Preset

Every agent runs on an opencode free model; no usage cost.

You need an API key for the `opencode` provider. Sign up at [OpenCode Zen](https://opencode.ai/zen) (skip billing; the free models need no balance) and connect it with `/connect`.

---

## The Config

```jsonc
{
  "preset": "opencode-zen-free",
  "presets": {
    "opencode-zen-free": {
      "orchestrator": {
        "model": "opencode/hy3-free",
        "temperature": 0.4,
        "skills": ["*"],
        "mcps": ["*", "!context7"]
      },
      "oracle": {
        "model": "opencode/big-pickle",
        "temperature": 0.4,
        "variant": "max",
        "skills": ["simplify"],
        "mcps": []
      },
      "explorer": {
        "model": "opencode/deepseek-v4-flash-free",
        "temperature": 0.2,
        "skills": [],
        "mcps": []
      },
      "librarian": {
        "model": "opencode/deepseek-v4-flash-free",
        "temperature": 0.2,
        "skills": [],
        "mcps": ["context7", "gh_grep"]
      },
      "designer": {
        "model": "opencode/mimo-v2.5-free",
        "temperature": 0.3,
        "variant": "medium",
        "skills": [],
        "mcps": []
      },
      "fixer": {
        "model": "opencode/laguna-s-2.1-free",
        "temperature": 0.2,
        "skills": [],
        "mcps": []
      },
      "observer": {
        "model": "opencode/mimo-v2.5-free",
        "temperature": 0.2,
        "variant": "low",
        "skills": [],
        "mcps": []
      }
    }
  }
}
```

## Skill Reference

| Skill | Description | Source |
| --- | --- | --- |
| `*` | All installed skills (wildcard) | `public` |
| `simplify` | Code simplification | `public` |

For the complete configuration reference, see [Configuration](configuration.md).
