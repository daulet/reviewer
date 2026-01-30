## Setup

If you are running `claude` sandboxed, at the very least enable the following permissions and excluded commands (`~/.claude/settings.json`):
```
{
  "permissions": {
    "allow": [
      "Read(path:~/.config/reviewer/**)",
      "WebFetch(domain:api.github.com)",
      "WebFetch(domain:github.com)",                                                                                              
      "WebFetch(domain:raw.githubusercontent.com)"
    ]
  },
  "model": "opus",
  "enabledPlugins": {
    "rust-analyzer-lsp@claude-plugins-official": true
  },
  "sandbox": {
    "enabled": true,
    "autoAllowBashIfSandboxed": true,
    "excludedCommands": ["gh"]
  }
}
```