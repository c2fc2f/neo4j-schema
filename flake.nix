{
  description = "A Rust CLI tool that introspects Neo4j databases to generate human-readable schema";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default-linux";
  };

  outputs =
    {
      self,
      nixpkgs,
      systems,
      ...
    }:
    let
      inherit (nixpkgs) lib;
      eachSystem = lib.genAttrs (import systems);

      pkgsFor = eachSystem (
        system:
        import nixpkgs {
          localSystem = system;
        }
      );
    in
    {
      packages = eachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = self.packages.${system}.neo4j-schema;

          neo4j-schema = pkgs.callPackage ./nix/package.nix {
            version = self.rev or self.dirtyRev or "dirty";
          };
        }
      );

      defaultPackage = eachSystem (system: self.packages.${system}.default);

      devShells = eachSystem (system: {
        default =
          pkgsFor.${system}.mkShell.override
            {
              inherit (self.packages.${system}.default) stdenv;
            }
            {
              env = {
                # Required by rust-analyzer
                RUST_SRC_PATH = "${pkgsFor.${system}.rustPlatform.rustLibSrc}";
              };

              nativeBuildInputs = with pkgsFor.${system}; [
                cargo
                rustc
                rust-analyzer
                rustfmt
                clippy

                rustPlatform.bindgenHook
              ];

              buildInputs = [ ];
            };
      });
    };
}
