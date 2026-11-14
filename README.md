# nowplayingd

Expose this Linux PC/laptop's media player (anything MPRIS: Spotify, Firefox, mpv, VLC…) as a
`media_player` entity in Home Assistant. Metadata and controls go through `playerctl`.

## Build & run

```sh
sudo apt install playerctl        # or pacman -S playerctl, dnf install playerctl
cargo build --release
./target/release/nowplayingd --url http://homeassistant.local:8123 --app-psw <TOKEN>
```

`--app-psw` is a Home Assistant **long-lived access token** (HA → your profile → Security →
Long-lived access tokens → Create token).

| Option | Default | |
|---|---|---|
| `--url` / `HA_URL` | – | HA base URL (http or https) |
| `--app-psw` / `HA_TOKEN` | – | long-lived access token |
| `--app-psw-file` / `HA_TOKEN_FILE` | – | read the token from a file instead |
| `--name` | hostname | entity becomes `media_player.<name>` |
| `--friendly-name` | hostname | display name in HA |
| `--player` | all | restrict to a player, e.g. `spotify` (passed to `playerctl --player`) |
| `--interval` | 1000 | poll interval in ms |

| `--config` | `~/.config/nowplayingd/config.toml` | use another config file |

## Config file

Instead of flags, put your settings in `~/.config/nowplayingd/config.toml` (or
`$XDG_CONFIG_HOME/nowplayingd/config.toml`; if neither exists, `/etc/nowplayingd/config.toml` is used). Any flag you pass still overrides the file.

```toml
url = "http://homeassistant.local:8123"
app_psw = "your-long-lived-access-token"
# name = "my_laptop"
# friendly_name = "My Laptop"
# player = "spotify"
# interval = 1000
```

```sh
mkdir -p ~/.config/nowplayingd
cp config.example.toml ~/.config/nowplayingd/config.toml
chmod 600 ~/.config/nowplayingd/config.toml   # it holds your token
nowplayingd                                    # no flags needed
```

Priority: flags / `HA_URL` / `HA_TOKEN` env vars  >  config file  >  defaults.
Unknown keys in the file are reported as errors, so typos don't fail silently.

## What HA gets

State (`playing` / `paused` / `idle`, `off` when nowplayingd exits), title, artist, album, cover
art (http(s) art URLs only), duration/position (progress bar), volume, shuffle and repeat.

Controls: play, pause, play/pause, stop, next, previous, seek, volume set/up/down, shuffle and
repeat. These work from dashboards, the more-info dialog, automations and scripts, e.g.

```yaml
action: media_player.media_play_pause
target:
  entity_id: media_player.my_laptop
```

## How it works / caveats

No custom integration or MQTT is required. nowplayingd writes the entity through the REST API
(`POST /api/states/...`) and listens on the WebSocket API for `call_service` events that
target its entity, then runs the matching `playerctl` command.

* HA doesn't own the entity, so it has no unique_id. It can't be renamed or assigned to an
  area in the UI. Use `--name`/`--friendly-name` instead.
* HA itself may log "Referenced entities … are missing" when you press a button. That warning
  is harmless; nowplayingd still gets the command.
* If HA restarts, the entity reappears as soon as nowplayingd reconnects (within ~5s).

## Autostart (systemd user service)

`~/.config/systemd/user/nowplayingd.service`:

```ini
[Unit]
Description=Home Assistant media player bridge
After=graphical-session.target

[Service]
# reads ~/.config/nowplayingd/config.toml
ExecStart=%h/.cargo/bin/nowplayingd
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

```sh
cargo install --path .
systemctl --user daemon-reload && systemctl --user enable --now nowplayingd
```

## NixOS

### As a NixOS module (recommended)

The flake exports `nixosModules.default`. In your system flake:

```nix
{
  inputs.nowplayingd.url = "path:/home/you/src/nowplayingd";   # or a git URL
  inputs.nowplayingd.inputs.nixpkgs.follows = "nixpkgs";

  outputs = { nixpkgs, nowplayingd, ... }: {
    nixosConfigurations.mylaptop = nixpkgs.lib.nixosSystem {
      modules = [
        nowplayingd.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

Then in `configuration.nix`:

```nix
services.nowplayingd = {
  enable = true;
  url = "http://homeassistant.local:8123";
  app_psw_file = "/run/secrets/ha-token";   # or: app_psw = "...";  (lands in /nix/store!)
  daemon = true;                            # run as a systemd user service (default)
  # name = "my_laptop";  friendly_name = "My Laptop";  player = "spotify";  interval = 1000;
};
```

| Option | Default | |
|---|---|---|
| `enable` | `false` | install the package + `/etc/nowplayingd/config.toml` |
| `url` | – | HA base URL |
| `app_psw` | `null` | token inline. **World-readable in /nix/store** |
| `app_psw_file` | `null` | path to a file containing the token, read at runtime (sops-nix/agenix). Set exactly one of the two |
| `name`, `friendly_name`, `player` | `null` | as the CLI flags |
| `interval` | `1000` | poll interval (ms) |
| `daemon` | `true` | start automatically as a **systemd user service** for each logged-in user |
| `package` | this flake | override the package |

Notes:
* It's a *user* service on purpose. `playerctl` talks to players over your login session's
  D-Bus, which a system service can't see. Check it with `systemctl --user status nowplayingd`
  and `journalctl --user -u nowplayingd -f`.
* The token file must be readable by your user. With sops-nix, for example:
  `sops.secrets.ha-token.owner = "you";`
* With `daemon = false` you can still run `nowplayingd` by hand; it reads
  `/etc/nowplayingd/config.toml`. A personal `~/.config/nowplayingd/config.toml` takes
  precedence over the system one.

### Just the package

```sh
nix run . -- --url http://homeassistant.local:8123 --app-psw <TOKEN>
nix profile install .
```

Flakes only see files tracked by git, so `git add` new files before building.
