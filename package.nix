#### Package ####

{
  lib,
  rustPlatform,
  pkg-config,
  SDL2,
  inter,
  imagemagick,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "steam-maintenance";
  version = "0.1.0";

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

  nativeBuildInputs = [
    pkg-config
    imagemagick
  ];
  buildInputs = [ SDL2 ];

  ## Inter is compiled into the binary
  STEAM_MAINTENANCE_FONT_REGULAR = "${inter}/share/fonts/truetype/InterVariable.ttf";
  STEAM_MAINTENANCE_FONT_BOLD = "${inter}/share/fonts/truetype/Inter.ttc";

  ## Steam library artwork for the shortcut (portrait, landscape, hero, logo)
  postInstall = ''
    grid=$out/share/steam-maintenance/grid
    mkdir -p $grid
    font=${inter}/share/fonts/truetype/InterVariable.ttf
    label() { # size pointsize out
      magick -size "$1" xc:black -font "$font" -fill white -pointsize "$2" \
        -gravity center -annotate +0+0 "System\nUpdate" "$3"
    }
    label 600x900 96 $grid/portrait.png
    label 920x430 72 $grid/landscape.png
    label 1920x620 110 $grid/hero.png
    magick -size 640x360 xc:none -font "$font" -fill white -pointsize 72 \
      -gravity center -annotate +0+0 "System\nUpdate" $grid/logo.png
  '';

  meta = {
    description = "User-driven NixOS upgrades for Steam machines running Jovian gaming mode";
    homepage = "https://github.com/OptimoSupreme/steam-maintenance";
    license = lib.licenses.mit;
    mainProgram = "steam-maintenance";
    platforms = [ "x86_64-linux" ];
  };
})
