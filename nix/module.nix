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
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}
