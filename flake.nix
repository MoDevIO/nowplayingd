{
  description = "nowplayingd: expose this machine's media player to Home Assistant";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "nowplayingd";
          version = "0.3.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          # make sure playerctl is always found
          postInstall = ''
            wrapProgram $out/bin/nowplayingd --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.playerctl ]}
          '';
          meta.mainProgram = "nowplayingd";
        };
      });

      # services.nowplayingd = { enable = true; ... };
      nixosModules.default = import ./nix/module.nix self;
      nixosModules.nowplayingd = self.nixosModules.default;
    };
}
