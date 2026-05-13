{
  description = "MusicFS - FUSE filesystem for music with metadata overlay";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    git-hooks.url = "github:cachix/git-hooks.nix";
  };

  outputs = { self, nixpkgs, flake-utils, git-hooks }:
  flake-utils.lib.eachDefaultSystem (system:
  let
    pkgs = import nixpkgs {
      inherit system;
    };

    pre-commit-check = git-hooks.lib.${system}.run {
      src = ./.;
      hooks = {
        rustfmt = {
          enable = true;
          packageOverrides = {
            cargo = pkgs.cargo;
            rustfmt = pkgs.rustfmt;
          };
        };
        clippy = {
          enable = true;
          packageOverrides = {
            cargo = pkgs.cargo;
            clippy = pkgs.clippy;
          };
        };
      };
    };
  in {
    checks = {
      inherit pre-commit-check;
    };

    devShells.default = pkgs.mkShell rec {
      inherit (pre-commit-check) shellHook;

      buildInputs = with pkgs; [
        pre-commit
        gitleaks

        just

        pkg-config
        fuse3
        sqlite
        openssl

        rustc
        cargo
        cargo-watch
        cargo-nextest
        cargo-criterion
        rust-analyzer
        clippy
        rustfmt

        clang
        lld

        protobuf
        grpcurl
      ];
    };
  });
}
