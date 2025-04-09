{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };

  outputs = { self, nixpkgs }: {

    packages.aarch64-linux.yarn-berry-3-fetcher = nixpkgs.legacyPackages.aarch64-linux.callPackage (
      {
        rustPlatform,
        pkg-config,
        libzip,
        openssl,
        YARN_ZIP_SUPPORTED_CACHE_VERSION ? 8,
      }:

      rustPlatform.buildRustPackage {
        pname = "yarn-zip";
        version = "1.0.0";

        src = self;

        cargoLock = {
          lockFile = ./Cargo.lock;
          allowBuiltinFetchGit = true;
        };

        LIBZIP_SYS_USE_PKG_CONFIG = 1;
        inherit YARN_ZIP_SUPPORTED_CACHE_VERSION;

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

    packages.aarch64-linux.yarn-berry-4-fetcher = self.packages.aarch64-linux.yarn-berry-3-fetcher.override {
      libzip = nixpkgs.legacyPackages.aarch64-linux.libzip.override {
        zlib = nixpkgs.legacyPackages.aarch64-linux.zlib-ng.override { withZlibCompat = true; };
      };
      YARN_ZIP_SUPPORTED_CACHE_VERSION = 10;
    };

    packages.aarch64-linux.default = self.packages.aarch64-linux.yarn-berry-4-fetcher;

  };
}
