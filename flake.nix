# flake.nix — Expose agents-in-a-box skills as a Nix package
#
# Usage in consuming flakes:
#   inputs.agents-in-a-box.url = "github:stevengonsalvez/agents-in-a-box";
#   skillsPkg = agents-in-a-box.packages.${system}.skills;
#
# The skills package contains all skill directories from toolkit/packages/skills/.

{
  description = "agents-in-a-box — AI agent skills toolkit";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs, ... }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in {
      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          skills = pkgs.stdenvNoCC.mkDerivation {
            pname = "agents-in-a-box-skills";
            version = "2.0.0";
            src = ./toolkit/packages/skills;
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/skills
              cp -r $src/* $out/skills/
            '';
          };

          agents = pkgs.stdenvNoCC.mkDerivation {
            pname = "agents-in-a-box-agents";
            version = "2.0.0";
            src = ./toolkit/packages/agents;
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/agents
              cp -r $src/* $out/agents/
            '';
          };

          toolkit = pkgs.stdenvNoCC.mkDerivation {
            pname = "agents-in-a-box-toolkit";
            version = "2.0.0";
            src = ./toolkit;
            phases = [ "installPhase" ];
            installPhase = ''
              mkdir -p $out/toolkit
              cp -r $src/* $out/toolkit/
            '';
          };

          default = self.packages.${system}.skills;
        });
    };
}
