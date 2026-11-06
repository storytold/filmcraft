{
  description = "FilmCraft — video editor, a free alternative to Premiere Pro, in pure Rust";

  inputs = {
    # FilmCraft needs edition 2024 and rustc ≥ 1.95, so the flake tracks unstable. Consumers on a
    # release channel can retarget it: `inputs.filmcraft.inputs.nixpkgs.follows = "nixpkgs";`
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Optional build input of the repository: the fonts that the build embeds when `CRAFT_FONTS_DIR`
    # is set. Official releases always build with them; without them everything works but some
    # scripts have no glyphs. Pinned to the revision the release workflow uses — bump deliberately
    # (`nix flake update craft-fonts`).
    craft-fonts = {
      url = "github:storytold/craft-fonts/abb83316d96aa59c1cf64784289e378fe9fa5695";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      craft-fonts,
    }:
    let
      inherit (nixpkgs) lib;

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      # The AppStream metadata carries a release date. The flake source's own commit date is
      # reproducible, unlike "now"; a tarball or dirty tree without one falls back to the epoch.
      buildDate =
        let
          d = self.lastModifiedDate or "";
        in
        if builtins.stringLength d >= 8 then
          "${builtins.substring 0 4 d}-${builtins.substring 4 2 d}-${builtins.substring 6 2 d}"
        else
          "1970-01-01";

      packageArgs = {
        inherit craft-fonts buildDate;
      };

      forAllSystems = f: lib.genAttrs systems (system: f (import nixpkgs { inherit system; }) system);
    in
    {
      packages = forAllSystems (
        pkgs: _:
        let
          filmcraft = pkgs.callPackage ./nix/package.nix packageArgs;
        in
        {
          inherit filmcraft;
          default = filmcraft;
        }
      );

      # `nixpkgs.overlays = [ inputs.filmcraft.overlays.default ]` → `pkgs.filmcraft` everywhere.
      overlays.default = final: _prev: {
        filmcraft = final.callPackage ./nix/package.nix packageArgs;
      };

      apps = forAllSystems (
        _: system: {
          default = {
            type = "app";
            program = lib.getExe self.packages.${system}.filmcraft;
          };
        }
      );

      devShells = forAllSystems (
        pkgs: system:
        let
          filmcraft = self.packages.${system}.filmcraft;
        in
        {
          default = pkgs.mkShell (
            {
              # The package's own native inputs, so `cargo build` in the shell needs no extra setup.
              inputsFrom = [ filmcraft ];
              packages = with pkgs; [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
              ];
              # `cargo run` dlopens the Vulkan loader, libwayland and libxkbcommon like the packaged binary.
              LD_LIBRARY_PATH = lib.makeLibraryPath filmcraft.passthru.runtimeLibs;
              RUST_BACKTRACE = "1";
            }
            // lib.optionalAttrs (craft-fonts != null) {
              # Embedded fonts in `cargo run` too; build.rs reads this as CRAFT_FONTS_DIR.
              CRAFT_FONTS_DIR = toString craft-fonts;
            }
          );
        }
      );

      checks = forAllSystems (
        pkgs: system: {
          filmcraft = self.packages.${system}.filmcraft;

          # `nix fmt` formats the flake; this keeps it formatted.
          formatting = pkgs.runCommand "filmcraft-nix-formatting" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
            nixfmt --check ${./flake.nix} ${./nix}
            touch $out
          '';
        }
      );

      formatter = forAllSystems (pkgs: _: pkgs.nixfmt);
    };
}
