{
  lib,
  rustPlatform,
  pkgs,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "musicfs";
  version = "0.1.0";

  src = ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };
  cargoHash = lib.fakeHash;

  nativeBuildInputs = with pkgs; [ 
    pkg-config 
    protobuf 
  ];

  buildInputs = with pkgs; [
    openssl
    fuse3
    sqlite
  ];

  PROTOC = "${pkgs.protobuf}/bin/protoc";

  meta = {
    description = "MusicFS - FUSE filesystem for music with metadata overlay";
    homepage = "https://github.com/LichHunter/MusicFS";
    license = lib.licenses.unlicense;
    maintainers = [ ];
  };
})
