{
  description = "jqc: compile jq filters to native Rust code via a proc-macro";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    typix = {
      url = "github:loqusion/typix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, typix }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);

      # Every `todo-reports/<name>/report.typ` becomes its own Typst project,
      # built via typix (https://github.com/loqusion/typix) for reproducible,
      # hermetic compilation.
      reportNames =
        let
          lib = nixpkgs.lib;
          entries = builtins.readDir ./todo-reports;
        in
        builtins.attrNames (lib.filterAttrs
          (name: kind:
            kind == "directory" && builtins.pathExists (./todo-reports + "/${name}/report.typ"))
          entries);

      reportPackages = system:
        let
          typixLib = typix.lib.${system};
          src = typixLib.cleanTypstSource ./todo-reports;
        in
        nixpkgs.lib.genAttrs
          (map (name: "report-${name}") reportNames)
          (pkgAttr:
            let name = nixpkgs.lib.removePrefix "report-" pkgAttr;
            in typixLib.buildTypstProject {
              inherit src;
              typstSource = "${name}/report.typ";
              typstOpts.root = ".";
            });
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
              pkgs.typst
            ];
            RUST_BACKTRACE = "1";
          };
        });

      # `nix run .#build-reports` compiles every todo-reports/<name>/report.typ
      # (each a typix-built derivation) and copies the PDF next to its source.
      apps = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          reports = reportPackages system;
        in {
          build-reports = {
            type = "app";
            program = toString (pkgs.writeShellScript "build-reports" ''
              set -euo pipefail
              cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"
              ${nixpkgs.lib.concatStrings (map (name: ''
                install -Dm644 "${reports."report-${name}"}" "todo-reports/${name}/report.pdf"
                echo "built todo-reports/${name}/report.pdf"
              '') reportNames)}
            '');
          };
        });

      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in (reportPackages system) // {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "jqc";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
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
