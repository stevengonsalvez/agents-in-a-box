{
  description = "reflect-kb — universal cross-harness retrieval + learning KB for AI coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;
        python = pkgs.python311;
        pyPkgs = python.pkgs;

        # ── nano-vectordb ──────────────────────────────────────────────────
        # Not packaged in nixpkgs. Pure-python, numpy-only dep chain.
        nano-vectordb = pyPkgs.buildPythonPackage rec {
          pname = "nano-vectordb";
          version = "0.0.4.3";
          pyproject = true;
          src = pkgs.fetchPypi {
            pname = "nano_vectordb";
            inherit version;
            # TOFU: replace with the hash nix reports on first build failure.
            hash = lib.fakeHash;
          };
          build-system = [ pyPkgs.setuptools ];
          dependencies = [ pyPkgs.numpy ];
          doCheck = false;
          pythonImportsCheck = [ "nano_vectordb" ];
        };

        # ── nano-graphrag (with broken chain stripped) ─────────────────────
        # Upstream install_requires pulls graspologic → hyppo → numba →
        # llvmlite, which only builds on py<3.10. We ship graspologic_shim.py
        # at runtime (pure networkx) to satisfy nano-graphrag's imports, so
        # we strip those declared deps here. `pythonRemoveDeps` rewrites the
        # dist-info install_requires so pip's runtime dep check passes inside
        # the reflect-kb buildPythonApplication below.
        nano-graphrag = pyPkgs.buildPythonPackage rec {
          pname = "nano-graphrag";
          version = "0.0.8.2";
          pyproject = true;
          src = pkgs.fetchPypi {
            pname = "nano_graphrag";
            inherit version;
            hash = lib.fakeHash;  # TOFU: fill on first build
          };
          build-system = [ pyPkgs.setuptools ];
          pythonRemoveDeps = [
            "graspologic"
            "hyppo"
            "numba"
            "llvmlite"
            "future"
          ];
          # The surviving set is the subset reflect-kb actually imports — see
          # global-learnings/cli/requirements.txt (authoritative) and the
          # reflect-kb pyproject.toml `dependencies` block.
          dependencies = with pyPkgs; [
            openai
            tenacity
            hnswlib
            xxhash
            networkx
            tiktoken
            pyyaml
            aiohttp
            tqdm
            nano-vectordb
          ];
          doCheck = false;
          # Skip import check — nano-graphrag imports graspologic at top
          # level and only our runtime shim can satisfy that. The real
          # import happens via reflect_kb.cli.graspologic_shim before
          # any nano_graphrag code loads.
          pythonImportsCheck = [ ];
        };

        # ── Python environment with every reflect-kb runtime dep ───────────
        # Built once, shared by the app and the dev shell. All deps listed
        # explicitly so the dep graph is auditable.
        pythonEnv = python.withPackages (ps: with ps; [
          # CLI
          click
          rich
          pyyaml
          httpx
          # Embeddings + numerics
          sentence-transformers
          numpy
          # Graph + tokenisation
          networkx
          tiktoken
          # nano-graphrag import-time deps (normally transitive, we ship
          # them directly because we install nano-graphrag with stripped
          # deps; see comment above).
          openai
          tenacity
          hnswlib
          xxhash
          # The overridden Python packages.
          nano-vectordb
          nano-graphrag
        ]);

        # ── reflect-kb application ─────────────────────────────────────────
        # Base CLI deps + the full `[graph]` extra so `nix run .#reflect-kb`
        # gets a working `reflect search` out of the box. pipx users on
        # macOS/Linux skip the heavy chain (see pyproject.toml comment).
        reflect-kb = pyPkgs.buildPythonApplication {
          pname = "reflect-kb";
          version = "0.1.0";
          pyproject = true;
          src = ./.;
          build-system = [ pyPkgs.hatchling ];
          dependencies = with pyPkgs; [
            # base
            click
            rich
            pyyaml
            httpx
            # [graph] extra
            sentence-transformers
            numpy
            networkx
            tiktoken
            openai
            tenacity
            hnswlib
            xxhash
            nano-vectordb
            nano-graphrag
          ];
          doCheck = false;
          pythonImportsCheck = [ "reflect_kb" ];
          meta = with lib; {
            description = "Universal cross-harness retrieval + learning KB for AI coding agents";
            homepage = "https://github.com/stevengonsalvez/reflect-kb";
            license = licenses.mit;
            mainProgram = "reflect";
          };
        };
      in {
        packages = {
          default = reflect-kb;
          reflect-kb = reflect-kb;
        };

        apps.default = {
          type = "app";
          program = "${reflect-kb}/bin/reflect";
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pythonEnv
            pkgs.uv
            pkgs.git
            pkgs.gh
            pkgs.pre-commit
          ];

          shellHook = ''
            echo "reflect-kb dev shell (python=${python.version})"
            echo "nano-graphrag override active — graspologic/hyppo/numba/llvmlite stripped."
            echo "For editable install over the nix env:  pip install -e . --no-deps --prefix $PWD/.venv-editable"
            echo "Spec: plans/reflect-v4-universal-install-spec.md"
          '';
        };
      });
}
