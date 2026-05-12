{
  description = "beetfs - FUSE filesystem for beets with metadata overlay (Python 2.7)";

  inputs = {
    # Using nixos-18.09 - has all Python 2 packages: fuse, mutagen, jellyfish, munkres
    # Mark as non-flake since 18.09 predates flakes
    nixpkgs-old = {
      url = "github:NixOS/nixpkgs/nixos-18.09";
      flake = false;
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs-old, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs-old {
          inherit system;
        };

        # Build beets 1.4.9 for Python 2 (last Python 2 compatible version)
        # Using minimal deps - autotagger features (munkres, jellyfish) not needed for beetfs
        beets-py2 = pkgs.python2Packages.buildPythonPackage rec {
          pname = "beets";
          version = "1.4.9";

          # Use legacy setup.py install (not pip/wheel)
          format = "other";

          src = pkgs.fetchFromGitHub {
            owner = "beetbox";
            repo = "beets";
            rev = "v${version}";
            sha256 = "sha256-KO5jtqxw82ylbSKsJgUGr3bfxUPH8vanf/+kv//CreM=";
          };

          # No patching needed - nixpkgs-18.09 has all required Python 2 packages

          nativeBuildInputs = with pkgs.python2Packages; [ setuptools ];

          buildPhase = "true";  # No build needed

          installPhase = ''
            # Direct copy to avoid dependency resolution
            mkdir -p $out/lib/python2.7/site-packages
            cp -r beets $out/lib/python2.7/site-packages/
            cp -r beetsplug $out/lib/python2.7/site-packages/

            # Create version file
            echo "__version__ = '${version}'" >> $out/lib/python2.7/site-packages/beets/__init__.py

            # Create beet command wrapper
            mkdir -p $out/bin
            cat > $out/bin/beet << 'EOF'
#!${pkgs.python2.interpreter}
import sys
from beets.ui import main
main()
EOF
            chmod +x $out/bin/beet
          '';

          propagatedBuildInputs = with pkgs.python2Packages; [
            pyyaml
            mutagen
            unidecode
            enum34
            six
            musicbrainzngs
            jellyfish
            munkres
          ];

          # Disable tests for faster builds
          doCheck = false;

          meta = {
            description = "Music library manager and tagger";
            homepage = "https://beets.io/";
          };
        };

        pythonEnv = pkgs.python2.withPackages (ps: with ps; [
          fuse
          mutagen
          pyyaml
          enum34
          six
          jellyfish
          munkres
          unidecode
          musicbrainzngs
        ] ++ [ beets-py2 ]);

      in {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pythonEnv
            pkgs.fuse
          ];

          shellHook = ''
            # Clear any system Python pollution and set clean PYTHONPATH
            unset PYTHONPATH
            export PYTHONPATH="$PWD/beetsplug"
            echo "beetfs development environment (Python 2.7)"
            echo "  Python: $(python --version 2>&1)"
            echo "  Run: python -c 'import beets; print(beets.__version__)'"
            echo ""
            echo "To mount: beet mount <mountpoint>"
          '';
        };
      }
    );
}
