{
  version,
  lib,
  installShellFiles,
  rustPlatform,
  buildFeatures ? [ ],
}:

rustPlatform.buildRustPackage {
  pname = "neo4j-schema";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../src
      ../Cargo.lock
      ../Cargo.toml
    ];
  };

  inherit buildFeatures;
  inherit version;

  # inject version from nix into the build
  env.NIX_RELEASE_VERSION = version;

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    installShellFiles

    rustPlatform.bindgenHook
  ];

  buildInputs = [ ];

  meta = with lib; {
    description = "A Rust CLI tool that introspects Neo4j databases to generate human-readable schema";
    mainProgram = "neo4j-schema";
    homepage = "https://github.com/c2fc2f/neo4j-schema";
    license = licenses.mit;
    maintainers = [ maintainers.c2fc2f ];
  };
}
