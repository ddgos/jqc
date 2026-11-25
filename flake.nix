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
      # second toolchain just to pin one package. `extraJqBins` (used only
      # by `default` below) additionally exposes every other pinned jq
      # version under its own name (`jq-1.6`, `jq-1.7`, …) on the same
      # PATH, so testing against 1.6/1.7/1.8 doesn't need switching
      # devShells -- one `nix develop` has all of them at once, e.g.
      # `jq-1.6 --version` or pointing `differential.rs`'s `run_real_jq`
      # at a specific one by hand.
      mkDevShell = system: jqPkg: extraJqBins:
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
            pkgs.cargo-criterion
            pkgs.gnuplot
            pkgs.just
          ] ++ extraJqBins;
          RUST_BACKTRACE = "1";
          # jqc-manual-tests-macros reads jq's own manual examples from
          # here (docs/content/manual/**/manual.yml) to generate
          # differential tests; see jq-src in `inputs` above.
          JQ_SRC = "${jq-src}";
        };

      # `name-${version}` (`jq-1_6`, …) -> a `pkgs.writeShellScriptBin`
      # wrapper exposing that version's `jq` binary under `jq-<version>`
      # (`jq-1.6`, with dots, matching how real jq reports `--version`)
      # instead of the bare `jq` every `oldJqInputs` package installs as.
      mkVersionedJqBin = system: versionLabel: input:
        let pkgs = nixpkgs.legacyPackages.${system};
        in pkgs.writeShellScriptBin "jq-${versionLabel}" ''
          exec "${input.legacyPackages.${system}.jq}/bin/jq" "$@"
        '';

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

      # Given a jq script file, returns a derivation whose `$out/bin/<name>`
      # is a standalone binary dedicated to that one filter -- the `nix
      # build`-time equivalent of `nix run . -- <script-file>`, for when the
      # compiled binary itself (not just a one-off run of it) is the thing
      # wanted. `name` is the script's file name without its `.jq` suffix.
      #
      # This builds `jqc-build` from source, then runs *it* as the
      # buildPhase (rather than letting `buildRustPackage` build `src`
      # itself) to compile the script -- `cargoLock.lockFile` still buys
      # the usual offline vendored-dependency setup, which covers the
      # `cargo build` `jqc-build` shells out to internally, since it
      # inherits the same `$CARGO_HOME`.
      mkJqScript = system: scriptFile:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          name = nixpkgs.lib.removeSuffix ".jq" (builtins.baseNameOf scriptFile);
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "jqc-script-${name}";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            cargo build --release -p jqc-build
            JQC_WORKSPACE_DIR=$PWD ./target/release/jqc-build ${scriptFile} -o ${name}
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            install -Dm755 ${name} $out/bin/${name}
            runHook postInstall
          '';
        };
    in
    {
      # `default` tracks nixpkgs-unstable's current jq (1.8.2). `jq-1_6`/
      # `jq-1_7` are the same shell with an older jq on PATH instead, e.g.
      # `nix develop .#jq-1_6 -c cargo test -p jqc --test differential`
      # runs the exact same differential suite against jq 1.6 as ground
      # truth, no test-code changes needed.
      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          versionedJqBins = nixpkgs.lib.mapAttrsToList
            (name: input: mkVersionedJqBin system (nixpkgs.lib.replaceStrings [ "_" ] [ "." ] (nixpkgs.lib.removePrefix "jq-" name)) input)
            oldJqInputs;
        in {
          default = mkDevShell system pkgs.jq versionedJqBins;
        } // nixpkgs.lib.mapAttrs
          (_: input: mkDevShell system input.legacyPackages.${system}.jq [ ])
          oldJqInputs);

      # `nix run .` (the default app) is `jqc-build`: `nix run . --
      # <script-file> [-o <output>]` compiles that jq filter into a
      # standalone binary, same as `nix build`-ing `mkJqScript` above but
      # without naming a derivation first. `nix run .#build-reports`
      # compiles every todo-reports/<name>/report.typ (each a typix-built
      # derivation) and copies the PDF next to its source. `examples`'
      # `compiled-filter` demo binary is still built as part of `default`
      # below and runnable via `nix run .#default-example` -- it's just
      # not what bare `nix run .` means, since it's one hardcoded filter
      # rather than the general-purpose tool.
      apps = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          reports = reportPackages system;
        in {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/jqc-build";
          };
          default-example = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/compiled-filter";
          };
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
            # The differential test suite (checkPhase runs `cargo test`)
            # shells out to `jq` as ground truth -- without this, `nix
            # build` fails that it's not on the sandboxed builder's PATH
            # even though `nix develop -c cargo test` works fine (the dev
            # shell provides `jq` itself; `nativeCheckInputs` is the
            # equivalent for the package build's check phase).
            nativeCheckInputs = [ pkgs.jq ];
            # `jqc-build` generates a scratch Cargo project per script and
            # shells out to `cargo build` on it at *its own* runtime (see
            # jqc-build/src/main.rs), so it needs cargo/rustc *and* a C
            # linker on PATH (rustc calls out to `cc` to link the final
            # binary -- `nix develop`'s `mkShell` provides one via
            # `stdenv` for free, which masked this when only testing
            # inside the dev shell; a `wrapProgram`-wrapped binary gets
            # nothing beyond what's explicitly listed here), plus a way
            # to find the `jqc` crate to depend on once its own build
            # sandbox is long gone -- `self` is a permanent store path,
            # unlike anything under $TMPDIR during this build.
            postInstall = ''
              wrapProgram $out/bin/jqc-build \
                --set JQC_WORKSPACE_DIR ${self} \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.cargo pkgs.rustc pkgs.stdenv.cc ]}
            '';
          };
          # A concrete, built-in demonstration that `mkJqScript` works:
          # `nix build .#example-script` produces a binary dedicated to
          # jqc-build/examples/select-active.jq, reproducibly and offline.
          example-script = mkJqScript system ./jqc-build/examples/select-active.jq;
        });

      lib = {
        inherit mkJqScript;
      };

      # `nix flake check` *builds* every `checks.*` output (unlike
      # `packages`/`apps`, which it only evaluates) -- so this is what
      # actually makes "run the test suite" part of `nix flake check`,
      # per README.md's Testing section:
      # - `default` re-uses `packages.default`: its checkPhase is already
      #   `cargo test --workspace` (differential, version_selection,
      #   fuzz_inputs, the jqc-compiler scaling-check regression guards,
      #   doc-tests -- everything README.md lists as part of a plain
      #   `cargo test --workspace`).
      # - `manual-tests` / `c-tests` cover the two workspace-excluded
      #   suites (jqc-manual-tests, jqc-c-tests -- both excluded because
      #   they need `$JQ_SRC` at *compile* time, which a plain
      #   `--workspace` run can't assume).
      # Deliberately not included: `fuzz_scripts -- --ignored` (README
      # calls out as slow/off-by-default) and the wall-time/peak-RSS
      # benchmarks under `--ignored` (diagnostics, not pass/fail checks).
      checks = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = self.packages.${system}.default;
          manual-tests = pkgs.rustPlatform.buildRustPackage {
            pname = "jqc-manual-tests";
            version = "0.1.0";
            src = self;
            sourceRoot = "source/jqc-manual-tests";
            cargoLock.lockFile = ./jqc-manual-tests/Cargo.lock;
            nativeCheckInputs = [ pkgs.jq ];
            # Needed at compile time, not just test-run time: the
            # generate_manual_tests! proc macro reads manual.yml from
            # here while expanding, during `cargo test`'s own build step.
            JQ_SRC = "${jq-src}";
            dontBuild = true;
            installPhase = "mkdir -p $out";
          };
          c-tests = pkgs.rustPlatform.buildRustPackage {
            pname = "jqc-c-tests";
            version = "0.1.0";
            src = self;
            sourceRoot = "source/jqc-c-tests";
            cargoLock.lockFile = ./jqc-c-tests/Cargo.lock;
            nativeCheckInputs = [ pkgs.jq ];
            # As with manual-tests above: generate_c_tests! reads jq.test
            # from here while expanding, during `cargo test`'s build step.
            JQ_SRC = "${jq-src}";
            dontBuild = true;
            installPhase = "mkdir -p $out";
          };
        });
    };
}
