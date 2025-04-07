{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };

  outputs = { self, nixpkgs }: {

    packages.aarch64-linux.yarn-zip-3 = nixpkgs.legacyPackages.aarch64-linux.callPackage (
      {
        rustPlatform,
        pkg-config,
        libzip,
        openssl,
        zlib-ng,
        fetchFromGitHub,
        fetchurl,
        fetchpatch,
        autoreconfHook,
        YARN_ZIP_SUPPORTED_LOCKFILE_VERSION ? 6,
      }:

      rustPlatform.buildRustPackage {
        pname = "yarn-zip";
        version = "1.0.0";

        src = self;

        cargoLock.lockFile = ./Cargo.lock;

        LIBZIP_SYS_USE_PKG_CONFIG = 1;
        inherit YARN_ZIP_SUPPORTED_LOCKFILE_VERSION;

        nativeBuildInputs = [
          rustPlatform.bindgenHook
          pkg-config
        ];

        buildInputs = [
          (libzip.overrideAttrs rec {
            patches = libzip.patches ++ [
              ./libzip-revert-to-old-versionneeded-behavior.patch
            ];
          })
          openssl
        ];
      }
    ) {};

    packages.aarch64-linux.yarn-zip-4 = self.packages.aarch64-linux.yarn-zip-3.override {
      libzip = nixpkgs.legacyPackages.aarch64-linux.libzip.override {
        zlib = nixpkgs.legacyPackages.aarch64-linux.zlib-ng.override { withZlibCompat = true; };
      };
      YARN_ZIP_SUPPORTED_LOCKFILE_VERSION = 8;
    };

    packages.aarch64-linux.default = self.packages.aarch64-linux.yarn-zip-4;

  };
}
