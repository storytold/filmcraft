{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  alsa-lib,
  libxkbcommon,
  wayland,
  vulkan-loader,
  libGL,
  libva,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  src,
}:

let
  runtimeLibs = [
    libxkbcommon
    wayland
    vulkan-loader
    libGL
    libva
    alsa-lib
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
rustPlatform.buildRustPackage {
  pname = "filmcraft";
  version = "0.8.0";

  inherit src;

  cargoLock = {
    lockFile = "${src}/Cargo.lock";
    outputHashes = {
      "craft-ui-0.1.0" = "sha256-VhLeT7VLdZAIz6gR9PdFI34IJxHsGF8NE0uzFCN2vIU=";
    };
  };

  cargoBuildFlags = [
    "-p" "filmcraft"
    "-p" "filmcraft-cli"
  ];

  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs = runtimeLibs;

  postInstall = ''
    install -Dm644 packaging/linux/ai.storyteller.filmcraft.desktop \
      $out/share/applications/ai.storyteller.filmcraft.desktop
    install -Dm644 packaging/linux/ai.storyteller.filmcraft.mime.xml \
      $out/share/mime/packages/ai.storyteller.filmcraft.xml
    mkdir -p $out/share/icons
    cp -r assets/app-icon/hicolor $out/share/icons/
  '';

  postFixup = ''
    wrapProgram $out/bin/filmcraft \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  passthru = { inherit runtimeLibs; };

  meta = {
    description = "Open-source Premiere Pro-style video editor written in pure Rust";
    homepage = "https://github.com/storytold/filmcraft";
    license = with lib.licenses; [ mit asl20 ];
    mainProgram = "filmcraft";
    platforms = lib.platforms.linux;
  };
}
