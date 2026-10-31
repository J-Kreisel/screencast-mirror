{
  description = "A small window mirror using the XDG ScreenCast portal";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      dependencies = pkgs: with pkgs; [
        gtk4
        gst_all_1.gstreamer
        gst_all_1.gst-plugins-base
        (gst_all_1.gst-plugins-rs.override { plugins = [ "gtk4" ]; })
        pipewire
      ];
    in {
      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "screencast-mirror";
            version = "0.1.0";
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml ./Cargo.lock ./src ./data ./LICENSE
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.pkg-config pkgs.wrapGAppsHook4 ];
            buildInputs = dependencies pkgs;

            postInstall = ''
              install -Dm644 data/io.github.screencast_mirror.desktop \
                $out/share/applications/io.github.screencast_mirror.desktop
            '';

            meta = {
              description = "Mirror a window using the XDG ScreenCast portal";
              license = pkgs.lib.licenses.mit;
              platforms = systems;
              mainProgram = "screencast-mirror";
            };
          };
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/screencast-mirror";
          meta.description = self.packages.${system}.default.meta.description;
        };
      });

      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [ cargo rustc rustfmt clippy pkg-config ];
            buildInputs = dependencies pkgs;
          };
        });
    };
}
