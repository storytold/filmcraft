{
  description = "FilmCraft - open-source video editor in pure Rust";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      filmcraft = pkgs.callPackage ./nix/package.nix { src = self; };
    in
    {
      packages.${system} = {
        default = filmcraft;
        inherit filmcraft;
      };

      overlays.default = _: prev: {
        filmcraft = prev.callPackage ./nix/package.nix { src = self; };
      };

      devShells.${system}.default = pkgs.mkShell {
        inputsFrom = [ filmcraft ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath filmcraft.runtimeLibs;
      };
    };
}
