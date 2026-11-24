{
  description = "jqc: compile jq filters to native Rust code via a proc-macro";

  inputs = {
    # The dev shell's `jq` -- and the differential test suite's ground
    # truth -- tracks whatever jq version nixos-unstable currently ships
    # (1.8.2 as of writing). The two pinned inputs below provide older jq
    # versions reproducibly, for testing against them deliberately rather
    # than whatever happens to be newest.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixpkgs-jq-1-6.url = "github:NixOS/nixpkgs/nixos-23.05"; # last channel shipping jq 1.6
    nixpkgs-jq-1-7.url = "github:NixOS/nixpkgs/nixos-23.11"; # first channel shipping jq 1.7.1
    # jq's own source, pinned to match the default jq version above --
    # docs/content/manual/ in this checkout holds the manual's worked
    # examples for every jq release (v1.3 through this one), which
    # jqc-manual-tests generates differential tests from.
    jq-src = {
      url = "github:jqlang/jq/jq-1.8.2";
      flake = false;
    };
    typix = {
      url = "github:loqusion/typix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, nixpkgs-jq-1-6, nixpkgs-jq-1-7, jq-src, typix }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);

      # name -> flake input providing that jq version's `legacyPackages`.
      oldJqInputs = {
        "jq-1_6" = nixpkgs-jq-1-6;
        "jq-1_7" = nixpkgs-jq-1-7;
      };

      # Same dev-shell contents as `default`, just with `jq` swapped for a
      # specific version -- everything else (cargo, rustc, …) still comes
      # from the primary `nixpkgs` input, so this doesn't rebuild a whole
      # second toolchain just to pin one package.
      mkDevShell = system: jqPkg:
        let pkgs = nixpkgs.legacyPackages.${system};
        in pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.clippy
            jqPkg
            pkgs.typst
            pkgs.cargo-tarpaulin
          ];
          RUST_BACKTRACE = "1";
          # jqc-manual-tests-macros reads jq's own manual examples from
          # here (docs/content/manual/**/manual.yml) to generate
          # differential tests; see jq-src in `inputs` above.
          JQ_SRC = "${jq-src}";
        };

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
      # `default` tracks nixpkgs-unstable's current jq (1.8.2). `jq-1_6`/
      # `jq-1_7` are the same shell with an older jq on PATH instead, e.g.
      # `nix develop .#jq-1_6 -c cargo test -p jqc --test differential`
      # runs the exact same differential suite against jq 1.6 as ground
      # truth, no test-code changes needed.
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = mkDevShell system pkgs.jq;
        } // nixpkgs.lib.mapAttrs
          (_: input: mkDevShell system input.legacyPackages.${system}.jq)
          oldJqInputs);

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
        in (reportPackages system)
        // (nixpkgs.lib.mapAttrs (_: input: input.legacyPackages.${system}.jq) oldJqInputs)
        // {
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
