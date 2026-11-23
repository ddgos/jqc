{
  description = "jqc: compile jq filters to native Rust code via a proc-macro";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in
    {
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.rust-analyzer
              pkgs.rustfmt
              pkgs.clippy
              pkgs.jq
            ];
            RUST_BACKTRACE = "1";
          };
        });

      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "jqc";
            version = "0.1.0";
            src = self;
            cargoLock = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
            # jqc-build shells out to `cargo` at runtime to compile generated
            # projects into standalone binaries, so cargo needs to be on PATH.
            postInstall = ''
              wrapProgram $out/bin/jqc-build --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.cargo pkgs.rustc ]}
            '';
          };
        });
    };
}
