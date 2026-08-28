{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        yarn-berry-3-fetcher = pkgs.callPackage (
          {
            rustPlatform,
            pkg-config,
            libzip,
            openssl,
            YARN_ZIP_SUPPORTED_CACHE_VERSION ? 8,
          }:

          rustPlatform.buildRustPackage {
            pname = "yarn-berry-fetcher";
            version = "1.2.3";

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
              (libzip.overrideAttrs {
                patches = libzip.patches ++ [
                  ./libzip-revert-to-old-versionneeded-behavior.patch
                ];
              })
              openssl
            ];
          }
        ) { };
        yarn-berry-4-fetcher = yarn-berry-3-fetcher.override {
          libzip = pkgs.libzip.override {
            zlib =
              (pkgs.zlib-ng.overrideAttrs (old: {
                patches = old.patches or [ ] ++ [
                  # Yarn hashes the output of libzip(untar(tarball)), so the output of libzip
                  # needs to be an exact match across versions, and this commit changes the
                  # exact output. This is ridiculous, but such is life.
                  (pkgs.fetchpatch {
                    url = "https://github.com/zlib-ng/zlib-ng/commit/be819413be8a284b1827437006c0859644d0c367.patch";
                    revert = true;
                    hash = "sha256-rwRcNKpA2dMWkC6WRATDOCYCDDqqPvFJkQ6DLDohQd8=";
                  })
                ];
              })).override
                { withZlibCompat = true; };
          };
          YARN_ZIP_SUPPORTED_CACHE_VERSION = 10;
        };
      in
      {
        formatter = pkgs.nixfmt-rfc-style;
        packages = {
          inherit yarn-berry-4-fetcher yarn-berry-3-fetcher;
          default = yarn-berry-4-fetcher;
        };
      }
    );
}
