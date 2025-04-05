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
          libzip
        ];
      }

    ) {};

    packages.aarch64-linux.default = self.packages.aarch64-linux.hello;

  };
}
