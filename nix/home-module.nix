self:

{ config, lib, pkgs, ... }:

let
  cfg = config.services.tether;
  tether = self.packages.${pkgs.stdenv.hostPlatform.system}.tether;

  configFile = pkgs.writeText "tether-config.toml" ''
    idle_timeout = "${cfg.settings.idleTimeout}"
    scrollback_lines = ${toString cfg.settings.scrollbackLines}
    max_sessions = ${toString cfg.settings.maxSessions}
    socket_path = ""
  '';
in
{
  options.services.tether = {
    enable = lib.mkEnableOption "Tether user service";

    package = lib.mkOption {
      type = lib.types.package;
      default = tether;
      description = "The tether package to use.";
    };

    settings = {
      idleTimeout = lib.mkOption {
        type = lib.types.str;
        default = "24h";
        description = "Idle timeout for detached sessions.";
      };

      scrollbackLines = lib.mkOption {
        type = lib.types.int;
        default = 10000;
        description = "Number of scrollback lines per session.";
      };

      maxSessions = lib.mkOption {
        type = lib.types.int;
        default = 20;
        description = "Maximum number of concurrent sessions.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."tether/config.toml".source = configFile;

    systemd.user.services.tetherd = {
      Unit = {
        Description = "Tether daemon";
        After = [ "default.target" ];
      };

      Service = {
        ExecStart = "${cfg.package}/bin/tetherd --config %h/.config/tether/config.toml";
        Restart = "on-failure";
        RestartSec = 5;
      };

      Install = {
        WantedBy = [ "default.target" ];
      };
    };
  };
}
