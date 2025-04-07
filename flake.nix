{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };

  outputs = { self, nixpkgs }: {

    packages.aarch64-linux.hello = nixpkgs.legacyPackages.aarch64-linux.callPackage (

      {
        rustPlatform,
        pkg-config,
        libzip,
        openssl,
        zlib-ng,
        fetchFromGitHub,
        fetchurl,
        fetchpatch,
        autoreconfHook
      }:

      rustPlatform.buildRustPackage {
        pname = "yarn-zip";
        version = "1.0.0";

        src = self;

        cargoVendorDir = "";

        LIBZIP_SYS_USE_PKG_CONFIG = 1;

        nativeBuildInputs = [
          rustPlatform.bindgenHook
          pkg-config
        ];

        buildInputs = [
          ((libzip.override {
            zlib = zlib-ng.override { withZlibCompat = true; };
            /*.overrideAttrs (old: rec {
              version = "2.1.2";
              src = fetchFromGitHub {
                owner = "zlib-ng";
                repo = "zlib-ng";
                rev = version;
                hash = "sha256-6IEH9IQsBiNwfAZAemmP0/p6CTOzxEKyekciuH6pLhw=";
              };
            });*/
          }).overrideAttrs rec {
            #version = "1.8.0";
            #src = fetchurl {
            #  url = "https://libzip.org/download/libzip-${version}.tar.gz";
            #  #url = "https://www.nih.at/libzip/libzip-${version}.tar.gz";
            #  hash = "sha256-MO5VhowKaY08YASS8r6k62LFOEm89pbSGvXrZfPzg54=";
            #};
            patches = libzip.patches ++ [
              ./foo.patch /*
              (fetchpatch {
                url = "https://github.com/nih-at/libzip/commit/854a176cb512002e40cb2084d87dc3d6ea122c95.patch";
                revert = true;
                hash = "sha256-TccgEGsj8+ObT5/kU4eYGJ372BzNYBocEbExv3rwZLQ=";
              })*/
            ];
          })
          openssl
        ];
      }

    ) {};

    packages.aarch64-linux.default = self.packages.aarch64-linux.hello;

  };
}
