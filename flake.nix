{
  description = "Astera Wayland compositor development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            strictDeps = true;

            nativeBuildInputs = with pkgs; [
              pkg-config
              rustup
            ];

            buildInputs = with pkgs; [
              libinput
              libgbm
              libglvnd
              libxkbcommon
              mesa
              pixman
              seatd
              systemd
              wayland
              wayland-protocols
              libx11
              libxcursor
              libxi
              libxrandr
            ];

            RUSTC_VERSION = "stable";

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [
              libinput
              libgbm
              libglvnd
              libxkbcommon
              mesa
              pixman
              seatd
              systemd
              wayland
              libx11
              libxcursor
              libxi
              libxrandr
            ]);

            shellHook = ''
              export PATH="''${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

              if ! rustup toolchain list | grep -q "^$RUSTC_VERSION"; then
                rustup toolchain install "$RUSTC_VERSION" --profile minimal
              fi
            '';
          };
        }
      );
    };
}
