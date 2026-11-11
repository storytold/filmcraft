{
  description = "FilmCraft - open-source professional non-linear video editor in pure Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        version = cargoToml.workspace.package.version;

        runtimeLibs = with pkgs; lib.optionals stdenv.hostPlatform.isLinux [
          alsa-lib
          libxkbcommon
          wayland
          vulkan-loader
          libGL
          libx11
          libxcursor
          libxi
          libxrandr
          fontconfig
        ];
      in
      {
        packages.filmcraft = pkgs.rustPlatform.buildRustPackage {
          pname = "filmcraft";
          inherit version;

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "craft-docpath-0.1.0" = "sha256-g1rwBAp5bH9onea8zg772bDgG+G/edXV7cdEGiR8wKQ=";
              "craft-web-api-0.1.0" = "sha256-HB6oIO8qOj1t36/9yuXNssdHMMiVeJdidnm/uSFaFps=";
              "ecolor-0.37.0" = "sha256-xzgBk6K1X/MZ8WUlIdy+r/0cf1FDkhJJuwQHZX+WY9U=";
              "winit-0.30.13" = "sha256-+67bk7cqhM5VHqROL3PeghmhKJvlPIxhU73ay45vaxw=";
            };
          };

          postPatch = ''
            sed -i '/egui_demo_lib/d; /egui_inspection/d; /egui_system_font_provider/d' Cargo.toml
          '';

          buildAndTestSubpackages = [ "apps/filmcraft" "apps/filmcraft-cli" ];

          # Disable tests during nix build to save RAM and avoid OOM (tests are run in CI)
          doCheck = false;

          # Include natural voices feature like official packages
          buildFeatures = [ "filmcraft/neural-voices" ];

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];

          buildInputs = runtimeLibs ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin (with pkgs.darwin.apple_sdk.frameworks; [
            AppKit
            CoreAudio
            CoreFoundation
            CoreGraphics
            CoreVideo
            Foundation
            Metal
            Security
          ]);

          postInstall = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            install -Dm644 packaging/linux/ai.storyteller.filmcraft.desktop $out/share/applications/ai.storyteller.filmcraft.desktop
            install -Dm644 packaging/linux/ai.storyteller.filmcraft.mime.xml $out/share/mime/packages/ai.storyteller.filmcraft.xml
            
            mkdir -p $out/share/icons
            cp -r assets/app-icon/hicolor $out/share/icons/

            wrapProgram $out/bin/filmcraft \
              --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
          '';

          meta = with lib; {
            description = "Open-source professional non-linear video editor built in pure Rust";
            homepage = "https://github.com/storytold/filmcraft";
            license = with licenses; [ mit asl20 ];
            mainProgram = "filmcraft";
            platforms = platforms.unix;
          };
        };

        packages.default = self.packages.${system}.filmcraft;

        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.filmcraft;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ self.packages.${system}.filmcraft ];

          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            pkg-config
          ];

          shellHook = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            export LD_LIBRARY_PATH="${lib.makeLibraryPath runtimeLibs}:$LD_LIBRARY_PATH"
          '';
        };
      }
    );
}
