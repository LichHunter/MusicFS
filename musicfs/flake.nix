{
  description = "MusicFS - FUSE filesystem for music libraries";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            pkg-config
            fuse3
            sqlite
            openssl
            
            # Linker toolchain
            clang
            lld
            
            # Dev tools
            cargo-watch
            cargo-nextest
            cargo-criterion
            
            # gRPC tooling (Week 10+)
            protobuf
            grpcurl
          ];
          
          RUST_BACKTRACE = "1";
          RUST_LOG = "debug";
        };
      }
    );
}
