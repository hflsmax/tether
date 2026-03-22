{ self }:

{ lib, rustPlatform, pkg-config }:

rustPlatform.buildRustPackage {
  pname = "tether";
  version = "0.1.3";

  src = lib.cleanSource self;

  cargoLock.lockFile = "${self}/Cargo.lock";

  nativeBuildInputs = [ pkg-config ];

  meta = with lib; {
    description = "Persistent PTY session manager";
    license = licenses.mit;
    mainProgram = "tether";
  };
}
