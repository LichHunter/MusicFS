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
        embedme = {
          enable = true;
          name = "embedme";
          description = "Keep README code blocks in sync with source files";
          entry = "${pkgs.nodePackages.embedme}/bin/embedme";
          args = [ "README.md" ];
          pass_filenames = false;
          language = "system";
        };
      };
    };
  in {
    checks = {
      inherit pre-commit-check;
    };

    packages = rec {
        musicfs = pkgs.callPackage ./package.nix { };
        default = musicfs;
    };

    devShells.default = pkgs.mkShell {
      inherit (pre-commit-check) shellHook;

      buildInputs = with pkgs; [
        pre-commit
        gitleaks

        just
        opencode

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
        crates-lsp

        protobuf
        grpcurl

        nodePackages.embedme
      ];
    };
  });
}
