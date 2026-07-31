{
  description = "Wotbox — a Gazelle tracker and qBittorrent manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    inputs@{
      self,
      flake-parts,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      flake.nixosModules.default = import ./nix/module.nix { inherit self; };

      perSystem =
        { pkgs, lib, ... }:
        let
          pname = "wotbox";
          version = "0.1.0";
          pnpm = pkgs.pnpm_10;
          frontendSource = lib.cleanSourceWith {
            src = ./frontend;
            filter =
              path: type:
              let
                name = baseNameOf path;
              in
              !builtins.elem name [
                "node_modules"
                "dist"
              ];
          };
          frontend = pkgs.stdenv.mkDerivation (finalAttrs: {
            pname = "wotbox-ui";
            inherit version;
            src = frontendSource;

            nativeBuildInputs = [
              pkgs.nodejs_24
              pnpm
              pkgs.pnpmConfigHook
            ];

            pnpmDeps = pkgs.fetchPnpmDeps {
              inherit (finalAttrs) pname version src;
              inherit pnpm;
              fetcherVersion = 4;
              hash = "sha256-poP0K3zWIHmsOvwItTWBj5aSe0B6HzTtE7xOWoULHnU=";
            };

            buildPhase = ''
              runHook preBuild
              pnpm check
              pnpm test
              pnpm build
              runHook postBuild
            '';

            installPhase = ''
              runHook preInstall
              cp -R dist "$out"
              runHook postInstall
            '';
          });
          rustPackageFiles = lib.fileset.unions [
            ./Cargo.lock
            ./Cargo.toml
            ./build.rs
            ./src
          ];
          rustPackageSource = lib.fileset.toSource {
            root = ./.;
            fileset = rustPackageFiles;
          };
          rustTestSource = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              rustPackageFiles
              ./tests
            ];
          };
          cargoNix = pkgs.callPackage ./Cargo.nix { };
          crateOverridesFor =
            src:
            pkgs.defaultCrateOverrides
            // {
              wotbox = _: {
                inherit src;
                WOTBOX_UI_DIST = frontend;
                SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
                meta = {
                  description = "Gazelle tracker and qBittorrent manager";
                  homepage = "https://github.com/conroy-cheers/wotbox";
                  license = lib.licenses.agpl3Only;
                  mainProgram = "wotbox";
                  platforms = lib.platforms.unix;
                };
              };
            };
          package = cargoNix.rootCrate.build.override {
            crateOverrides = crateOverridesFor rustPackageSource;
          };
          packageTests = cargoNix.rootCrate.build.override {
            crateOverrides = crateOverridesFor rustTestSource;
            runTests = true;
          };
          updateCargoNix = pkgs.writeShellApplication {
            name = "wotbox-update-cargo-nix";
            runtimeInputs = [ pkgs.crate2nix ];
            text = "crate2nix generate";
          };
          devSleet = pkgs.writeShellApplication {
            name = "wotbox-dev-sleet";
            runtimeInputs = [
              pkgs.cargo
              pkgs.coreutils
              pkgs.libiconv
              pkgs.netcat
              pkgs.nodejs_24
              pkgs.openssh
              pnpm
              pkgs.rustc
              pkgs.stdenv.cc
            ];
            text = ''
              export SSL_CERT_FILE="${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              export LIBRARY_PATH="${pkgs.libiconv}/lib''${LIBRARY_PATH:+:''${LIBRARY_PATH}}"
              ${builtins.readFile ./scripts/dev-sleet.sh}
            '';
          };
        in
        {
          packages = {
            default = package;
            wotbox = package;
            frontend = frontend;
            dev-sleet = devSleet;
            update-cargo-nix = updateCargoNix;
          };

          apps = {
            default = {
              type = "app";
              program = lib.getExe package;
            };
            dev-sleet = {
              type = "app";
              program = lib.getExe devSleet;
            };
            update-cargo-nix = {
              type = "app";
              program = lib.getExe updateCargoNix;
            };
          };

          checks = {
            inherit frontend;
            package = packageTests;
          }
          // lib.optionalAttrs pkgs.stdenv.isLinux {
            module = pkgs.testers.runNixOSTest {
              name = "wotbox-module";
              nodes.machine =
                { ... }:
                {
                  imports = [ self.nixosModules.default ];
                  services.wotbox = {
                    enable = true;
                    trackers.ops = {
                      kind = "ops";
                      baseUrl = "https://example.invalid";
                      tokenFile = "/etc/wotbox-test/ops";
                    };
                    downloadClients.music = {
                      baseUrl = "http://127.0.0.1:8001";
                      apiKeyFile = "/etc/wotbox-test/qbit";
                    };
                    downloadProfiles.ops = {
                      client = "music";
                      savePath = "/downloads/ops";
                      tag = "ops";
                    };
                    plex = {
                      tokenFile = "/etc/wotbox-test/plex";
                      sectionId = 4;
                      libraryRoots = [ "/downloads/ops" ];
                    };
                  };
                  environment.etc = {
                    "wotbox-test/ops".text = "test-token";
                    "wotbox-test/qbit".text = "qbt_0123456789abcdefghijklmnopqr";
                    "wotbox-test/plex".text = "test-token";
                  };
                };
              testScript = ''
                machine.wait_for_unit("wotbox.service")
              '';
            };
          };

          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.crate2nix
              pkgs.nodejs_24
              pnpm
              pkgs.rust-analyzer
              pkgs.rustc
              pkgs.rustfmt
              pkgs.cargo-nextest
              pkgs.netcat
              pkgs.openssh
            ];
            RUST_LOG = "wotbox=debug,tower_http=info";
          };

          formatter = pkgs.nixfmt;
        };
    };
}
