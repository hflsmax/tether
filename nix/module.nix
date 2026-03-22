self:

{ config, lib, pkgs, ... }:

let
  cfg = config.services.tether;
  tether = self.packages.${pkgs.stdenv.hostPlatform.system}.tether;
in
{
  options.services.tether = {
    enable = lib.mkEnableOption "Tether persistent PTY session manager";

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
        description = "Maximum number of concurrent sessions per user.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    # System-wide config file
    environment.etc."tether/config.toml".text = ''
      idle_timeout = "${cfg.settings.idleTimeout}"
      scrollback_lines = ${toString cfg.settings.scrollbackLines}
      max_sessions = ${toString cfg.settings.maxSessions}
      socket_path = ""
    '';

    # Systemd user unit — runs per-user when any user logs in
    systemd.user.services.tetherd = {
      description = "Tether daemon";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        ExecStart = "${cfg.package}/bin/tetherd --config /etc/tether/config.toml";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}
