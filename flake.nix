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
          config.allowUnfree = true;
          config.android_sdk.accept_license = true;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
          targets = [ "aarch64-linux-android" ];
        };
        androidComposition = pkgs.androidenv.composeAndroidPackages {
          platformVersions = [ "35" ];
          buildToolsVersions = [ "34.0.0" "35.0.0" ];
          ndkVersions = [ "27.2.12479018" ];
          includeNDK = true;
          includeEmulator = false;
          includeSystemImages = false;
        };
        androidSdk = androidComposition.androidsdk;
        androidNdk = "${androidComposition.ndk-bundle}/libexec/android-sdk/ndk-bundle";
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo-audit
            pkgs.cargo-llvm-cov
            pkgs.cargo-ndk
            pkgs.gradle
            pkgs.python312
            pkgs.sqlite
            pkgs.pkg-config
            pkgs.openssl
            # Android SDK + NDK
            androidSdk
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
            # GPUI needs Vulkan, Wayland, and font libraries at runtime
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [
              pkgs.vulkan-loader
              pkgs.wayland
              pkgs.libxkbcommon
              pkgs.fontconfig
            ]}:$LD_LIBRARY_PATH"
            # Android SDK/NDK
            export ANDROID_HOME="${androidSdk}/libexec/android-sdk"
            export ANDROID_NDK_HOME="${androidNdk}"
          '';
        };

        # FHS shell for Android builds (pre-built binaries like aapt2 need /lib64)
        devShells.android = (pkgs.buildFHSEnv {
          name = "zremote-android";
          targetPkgs = p: [
            rustToolchain
            p.cargo-ndk
            p.gradle
            p.zlib
            p.glibc
            androidSdk
          ];
          profile = ''
            export ANDROID_HOME="${androidSdk}/libexec/android-sdk"
            export ANDROID_NDK_HOME="${androidNdk}"
          '';
        }).env;
      }
    );
}
