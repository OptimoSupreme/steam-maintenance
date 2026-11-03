#### Decky Plugin ####

{
  lib,
  stdenvNoCC,
  fetchurl,
  esbuild,
  typescript,
  steam-maintenance,
  version ? steam-maintenance.version,
}:

let
  ## The loader API is bundled in; the rest are typings for the compile check
  npm = {
    "@decky/api" = fetchurl {
      url = "https://registry.npmjs.org/@decky/api/-/api-1.1.3.tgz";
      hash = "sha512-XsPCZxfxk5I1UtylIUN3qaWQI31siQbKfbLIskkI5innEatY1m4NQqBv/6hwPaO9mKMbdqYpnh5PSJDeMEOOBA==";
    };
    "@decky/ui" = fetchurl {
      url = "https://registry.npmjs.org/@decky/ui/-/ui-4.16.5.tgz";
      hash = "sha512-3vefJUGf5eXW27H+TyU2seWQzJZB/I3/PJUNG6LfwNdvbjv6jCSAXkK+4A3ICT4c2BWjCVe+tZZf7JUq+ZDcfw==";
    };
    "@types/react" = fetchurl {
      url = "https://registry.npmjs.org/@types/react/-/react-19.1.1.tgz";
      hash = "sha512-ePapxDL7qrgqSF67s0h9m412d9DbXyC1n59O2st+9rjuuamWsZuD2w55rqY12CbzsZ7uVXb5Nw0gEp9Z8MMutQ==";
    };
    csstype = fetchurl {
      url = "https://registry.npmjs.org/csstype/-/csstype-3.2.3.tgz";
      hash = "sha512-z1HGKcYy2xA8AGQfwrn0PAy+PB7X/GSj3UVJW9qKyn43xWa+gl5nXmU4qqLMRzWVLFC8KusUX8T/0kCiOYpAIQ==";
    };
  };

  unpack = lib.concatStrings (
    lib.mapAttrsToList (name: tarball: ''
      mkdir -p node_modules/${name}
      tar xzf ${tarball} --strip-components=1 -C node_modules/${name}
    '') npm
  );

  packageJson = builtins.toJSON {
    name = "steam-maintenance";
    inherit version;
    type = "module";
    license = "MIT";
  };
in
stdenvNoCC.mkDerivation {
  pname = "steam-maintenance-decky";
  inherit version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./plugin.json
      ./main.py
      ./tsconfig.json
      ./src
    ];
  };

  nativeBuildInputs = [
    esbuild
    typescript
  ];

  ## react and @decky/ui are globals in the Steam UI (see src/globals.ts); only
  ## @decky/api is bundled, and it reads plugin.json as @decky/manifest
  buildPhase = ''
    runHook preBuild
    ${unpack}
    tsc -p tsconfig.json
    esbuild src/index.tsx --bundle --format=esm --target=es2020 \
      --jsx=transform --jsx-factory=React.createElement --jsx-fragment=React.Fragment \
      --alias:@decky/manifest=$PWD/plugin.json \
      --outfile=dist/index.js
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -r dist plugin.json $out/
    substitute main.py $out/main.py --subst-var-by steamMaintenance ${lib.getExe steam-maintenance}
    echo '${packageJson}' > $out/package.json
    runHook postInstall
  '';

  meta = {
    description = "Decky plugin for steam-maintenance";
    homepage = "https://github.com/OptimoSupreme/steam-maintenance";
    license = lib.licenses.mit;
    platforms = [ "x86_64-linux" ];
  };
}
