# Hotaru WebUI assets

These static assets are embedded into the Hotaru daemon at build time. Run the local WebUI with:

```sh
cargo run --bin hotaru -- web
```

The command starts the Hotaru daemon (the same `hotaru` executable re-run in daemon mode) when needed, prints the access URLs, and exits. Use `hotaru daemon status` or `hotaru daemon stop` to inspect or stop the daemon. WebUI listens on all local network interfaces by default. Password protection is optional:

```sh
cargo run --bin hotaru -- web -p secret
cargo run --bin hotaru -- web -p
cargo run --bin hotaru -- web --password-file /path/to/password.txt
```

With a password configured, the WebUI prompts for it and establishes a same-origin session after login.

## Theming

All colors are built on MD3 system tokens (`--md-sys-color-*`) defined at the top of
`styles.css`, with the legacy variable names (`--accent`, `--gold`, …) kept as aliases.
Two built-in themes derive from the Hotaru logo:

- **晨光 / dawn** (`data-theme="linen"`): warm cream surface, wisteria primary `#7568b0`
- **夜阑 / dusk** (`data-theme="graphite"`, default): evening blue surface, mist-blue primary `#aebde8`

Accent roles: secondary = hair gold (tool activity, model badge), tertiary = ribbon
crimson (active session marker, stop button), plus a semantic online-green
(`--md-ext-color-online`) that never follows wallpaper colors.

`index.html` loads `/theme.css` after `styles.css`; a matugen-generated override can be
served there to recolor the whole UI from the desktop wallpaper (see `extra/matugen/`).
The 404 when no override exists is harmless. Serving `~/.hotaru/config/webui-theme.css`
at `/theme.css` is a pending backend route.
