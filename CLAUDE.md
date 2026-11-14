# nowplayingd

Rust CLI that exposes a Linux machine's MPRIS media player (via the `playerctl` CLI) as a
`media_player.<name>` entity in Home Assistant. The user runs NixOS.

## Architecture (single file: `src/main.rs`)

- **Config**: `Cli` (clap) → `FileConfig` (`~/.config/nowplayingd/config.toml`, or
  `$XDG_CONFIG_HOME`, or `--config`) → merged into `Args`. Priority: CLI flags / env
  (`HA_URL`, `HA_TOKEN`) > config file > defaults. `--app-psw` is the HA long-lived access
  token (alias `--app-pws`; file key `app_psw`, aliases `app-psw`, `token`). Alternatively
  `--app-psw-file` / `HA_TOKEN_FILE` / `app_psw_file` (read at runtime). Precedence: CLI token >
  CLI token file > file token > file token file. Config lookup: `--config`, then
  `~/.config/...` if it exists, then `/etc/nowplayingd/config.toml` (written by the NixOS
  module). The file uses `deny_unknown_fields`.
- **State out**: `poll_loop` runs `playerctl metadata --format …` (fields split by `\x1f`)
  every `interval` ms → `Snap` → `ha_state()` → REST `POST /api/states/<entity_id>`.
  It pushes on change, on a position jump of more than 2s, on a 30s heartbeat, or when
  `force_push` is set.
- **Commands in**: `ws_loop`/`ws_session` authenticate on `/api/websocket`, then
  `subscribe_events` for `call_service`. Events for domain `media_player` that target our
  entity are mapped by `service_to_playerctl()` to a `playerctl` invocation, then `poke` the
  poll loop. It reconnects every 5s and forces a state push after reconnecting, because an
  HA restart wipes REST-created states.
- **Shutdown**: on SIGINT/SIGTERM it pushes state `off`.
- `supported_features` = the `FEATURES` bitmask (`MediaPlayerEntityFeature` values).

Known limitation: the entity is REST-created, so HA has no unique_id or registry entry for
it. HA logs "referenced entities missing" when you press a button, but the `call_service`
event still fires.

## Commands

- `cargo test`: unit tests (URL conversion, service mapping, targeting, name sanitizing)
- `cargo clippy`: keep it warning-free
- `tests/e2e/run.sh`: end-to-end test with a fake `playerctl` and a mock HA (needs python3 + aiohttp)
- Static binary (portable, runs on NixOS without nix-ld):
  `RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --target x86_64-unknown-linux-gnu`
  (static glibc can't use nss-mdns, so `.local` hostnames may not resolve there; use an IP)
- NixOS module: `nix/module.nix`, exported as `nixosModules.default`. It provides
  `services.nowplayingd.{enable,url,app_psw,app_psw_file,name,friendly_name,player,interval,daemon,package}`
  (snake_case names were chosen by the user). It generates a TOML config, installs it as
  `/etc/nowplayingd/config.toml`, and with `daemon = true` creates a systemd **user**
  service (it must be a user service: playerctl needs the session D-Bus). The module has
  only been syntax-checked, never evaluated; verify with
  `nix flake check` or by building a NixOS config.
- Nix: `nix build` / `nix run . -- --help` (`flake.nix` wraps the binary with playerctl on PATH)
