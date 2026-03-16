{
  description = "MyRemote development environment";

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
          ];

          shellHook = ''
            echo "MyRemote dev shell ready"
            echo "  Rust: $(rustc --version)"
            echo "  Cargo: $(cargo --version)"
            echo "  Bun: $(bun --version)"
          '';
        };
      }
    );
}
