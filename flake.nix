{
  description = "ZRemote development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo-audit
            pkgs.cargo-llvm-cov
            pkgs.bun
            pkgs.nodejs_22
            pkgs.python312
            pkgs.sqlite
            pkgs.pkg-config
            pkgs.openssl
            # GPUI dependencies (GPU-accelerated UI framework)
            pkgs.vulkan-loader
            pkgs.wayland
            pkgs.wayland-protocols
            pkgs.libxkbcommon
            pkgs.fontconfig
            pkgs.freetype
            pkgs.libxcb
            pkgs.alsa-lib
            pkgs.cmake
            # Headless screenshot testing (cage compositor + grim capture)
            pkgs.cage
            pkgs.grim
          ];

          shellHook = ''
            echo "ZRemote dev shell ready"
            echo "  Rust: $(rustc --version)"
            echo "  Cargo: $(cargo --version)"
            echo "  Bun: $(bun --version)"
            # GPUI needs Vulkan, Wayland, and font libraries at runtime
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
              pkgs.vulkan-loader
              pkgs.wayland
              pkgs.libxkbcommon
              pkgs.fontconfig
            ]}:$LD_LIBRARY_PATH"
          '';
        };
      }
    );
}
