#### Package ####

{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "steam-maintenance";
  version = "0.2.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      (lib.fileset.maybeMissing ./tests/fixtures)
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  meta = {
    description = "User-driven NixOS upgrades for Steam machines running Jovian gaming mode";
    homepage = "https://github.com/OptimoSupreme/steam-maintenance";
    license = lib.licenses.mit;
    mainProgram = "steam-maintenance";
    platforms = [ "x86_64-linux" ];
  };
})
