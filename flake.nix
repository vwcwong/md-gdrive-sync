{
  description = "Sync Markdown notes from git repositories into Google Drive for NotebookLM";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Single source of truth for the toolchain: rust-toolchain.toml.
        # Read here so `nix develop` and any rustup user agree on the version.
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.git
            pkgs.cargo-insta
            pkgs.cacert
          ];

          env = {
            RUST_BACKTRACE = "1";
            # reqwest is built with rustls, but the cert store still has to be
            # pointed at explicitly inside the pure-ish nix shell.
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          };
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
