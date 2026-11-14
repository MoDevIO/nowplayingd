# NixOS module: services.nowplayingd
#
#   services.nowplayingd = {
#     enable = true;
#     url = "http://homeassistant.local:8123";
#     app_psw_file = "/run/secrets/ha-token";   # or: app_psw = "...";
#     daemon = true;
#   };
#
# `self` is the flake, so the default package comes from this repo.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.nowplayingd;
  inherit (lib) mkOption mkEnableOption mkIf types optional;

  settings = lib.filterAttrs (_: v: v != null) {
    inherit (cfg) url app_psw app_psw_file name friendly_name player interval;
  };
  configFile = (pkgs.formats.toml { }).generate "nowplayingd.toml" settings;
in
{
  options.services.nowplayingd = {
    enable = mkEnableOption "nowplayingd, which exposes this machine's media player to Home Assistant";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "nowplayingd.packages.\${system}.default";
      description = "The nowplayingd package to use.";
    };

    url = mkOption {
      type = types.str;
      example = "http://homeassistant.local:8123";
      description = "Home Assistant base URL.";
    };

    app_psw = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        Home Assistant long-lived access token.
        WARNING: this ends up world-readable in /nix/store. Prefer `app_psw_file`.
      '';
    };

    app_psw_file = mkOption {
      # deliberately a string, not a path: a path would be copied into the store
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/ha-token";
      description = ''
        File containing the access token, read at runtime (e.g. a sops-nix or agenix secret).
        Must be readable by the user(s) the service runs as.
      '';
    };

    name = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "my_laptop";
      description = "Entity object id (media_player.<name>). Defaults to the hostname.";
    };

    friendly_name = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "My Laptop";
      description = "Display name in Home Assistant. Defaults to the hostname.";
    };

    player = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "spotify";
      description = "Only follow this player (playerctl --player syntax).";
    };

    interval = mkOption {
      type = types.ints.positive;
      default = 1000;
      description = "Poll interval in milliseconds.";
    };

    daemon = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Run nowplayingd automatically as a systemd user service for every user who logs in.
        It has to be a user service: playerctl needs the login session's D-Bus.
        If false, the package and config are still installed and you can run `nowplayingd` by hand.
      '';
    };
  };

  config = mkIf cfg.enable {
    assertions = [{
      assertion = (cfg.app_psw != null) != (cfg.app_psw_file != null);
      message = "services.nowplayingd: set exactly one of `app_psw` or `app_psw_file`.";
    }];

    warnings = optional (cfg.app_psw != null)
      "services.nowplayingd.app_psw puts your Home Assistant token in the world-readable /nix/store; consider app_psw_file.";

    environment.systemPackages = [ cfg.package ];

    # picked up automatically when running `nowplayingd` by hand
    # (a user's ~/.config/nowplayingd/config.toml still takes precedence)
    environment.etc."nowplayingd/config.toml".source = configFile;

    systemd.user.services.nowplayingd = mkIf cfg.daemon {
      description = "Expose media player to Home Assistant (nowplayingd)";
      wantedBy = [ "default.target" ];
      after = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --config ${configFile}";
        Restart = "always";
        RestartSec = 5;
      };
    };
  };
}
