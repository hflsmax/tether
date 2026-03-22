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

    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = lib.attrNames (lib.filterAttrs (_: u: u.isNormalUser) config.users.users);
      description = "Users to run tetherd for. Defaults to all normal users.";
      example = [ "alice" "bob" ];
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

    environment.etc."tether/config.toml".text = ''
      idle_timeout = "${cfg.settings.idleTimeout}"
      scrollback_lines = ${toString cfg.settings.scrollbackLines}
      max_sessions = ${toString cfg.settings.maxSessions}
      socket_path = ""
    '';

    # One system service per user, starts at boot (before any SSH/tether connection)
    systemd.services = lib.listToAttrs (map (user: {
      name = "tetherd-${user}";
      value = {
        description = "Tether daemon for ${user}";
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          User = user;
          ExecStart = "${cfg.package}/bin/tetherd --config /etc/tether/config.toml";
          Restart = "on-failure";
          RestartSec = 5;
          RuntimeDirectory = "user/%U";
        };
      };
    }) cfg.users);
  };
}
