---
layout: home

hero:
  name: Kairos
  text: Press a hotkey, type, press Enter.
  tagline: A keyboard launcher for Linux and Windows. One native binary with low resource usage.
  image:
    src: /logo.png
    alt: Kairos
  actions:
    - theme: brand
      text: Install
      link: /guide/installation
    - theme: alt
      text: Plugins
      link: /plugins/
    - theme: alt
      text: Write a plugin
      link: /develop/writing-plugins

features:
  - title: Apps, files, windows
    details: Launch applications ranked by how often you use them, find files, switch to open windows, kill processes.
    link: /plugins/apps
  - title: Calculator and conversions
    details: "= 2+2, 5 km to miles, 100 usd to chf: math, units and currencies as you type."
    link: /plugins/calc
  - title: Ask an AI
    details: Anthropic or any OpenAI-compatible API with your own key, follow-up questions and tools from MCP servers.
    link: /plugins/ai
  - title: Your own commands
    details: Keyword commands from config.toml, or scripts with a comment header (Raycast script commands work too).
    link: /plugins/scripts
  - title: Plugins in any language
    details: External plugins talk JSON-RPC over stdin/stdout; remote plugins are HTTPS services.
    link: /develop/writing-plugins
  - title: Stays out of the way
    details: Software-rendered UI, plugins stopped while idle, updates installed while the launcher is hidden.
    link: /guide/configuration
---

## What you can type

| Type | Does | Plugin |
|---|---|---|
| `firefox` | Launch applications, ranked by how often and how recently you use them | [Applications](/plugins/apps) |
| `f report` | Find files and folders; `f ~/Documents/` browses | [Files](/plugins/files) |
| `= 2+2`, `5 km to miles`, `100 usd to chf` | Calculator, unit and currency conversion | [Calculator](/plugins/calc) |
| `ai what is a lifetime` | Ask an AI, with follow-ups and tools from MCP servers | [AI](/plugins/ai) |
| `> htop` | Run a shell command in a terminal | [Shell](/plugins/shell) |
| `ssh web1` | Connect to hosts from `~/.ssh/config` | [SSH](/plugins/ssh) |
| `scripts create backup`, `jira PROJ-1` | Your own scripts as commands | [Script commands](/plugins/scripts) |
| `win firefox` | Switch to or close open windows | [Windows](/plugins/windows) |
| `sys lock` | Lock, sleep, log out, restart, shut down | [System](/plugins/system) |
| `kill firefox` | Terminate or force-kill your own processes | [Processes](/plugins/processes) |
| `web rust`, `g rust` | Web searches and typed URLs | [Web Search](/plugins/web) |
| `:thumbs`, `emoji party` | Find an emoji and copy it | [Emoji](/plugins/emoji) |
| `uuid`, `genpw 32`, `hash sha256 hi` | UUIDs, passwords, URL/Base64 encoding, hashes | [Dev utilities](/plugins/devtools) |
| `github`, `bm rust docs` | Open browser bookmarks | [Bookmarks](/plugins/bookmarks) |
| `timer 10m tea` | Timers and alarms with a notification | [Timer](/plugins/timer) |
| `plugin` | Enable, configure, install and remove plugins | [Plugins](/plugins/#managing-plugins) |
| `update` | Check for a new version and install it | [Update](/plugins/update) |
