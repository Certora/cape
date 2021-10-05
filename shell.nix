with import ./nix/nixpkgs.nix { };

let
  mySolc = callPackage ./nix/solc-bin { version = "0.8.4"; };
  myPython = [
    poetry
    (poetry2nix.mkPoetryEnv {
      projectDir = ./.;
    })
  ];
in
mkShell
{
  buildInputs = [
    go-ethereum
    nodePackages.pnpm
    mySolc
    hivemind # process runner
    nodejs-12_x # nodejs
    jq
    entr # watch files for changes, for example: ls contracts/*.sol | entr -c hardhat compile
  ] ++ myPython;

  shellHook = ''
    export TEST_MNEMONIC="test test test test test test test test test test test junk"
    export SOLC_VERSION=${mySolc.version}
    export SOLC_PATH=${mySolc}/bin/solc
    export PATH=$(pwd)/bin:$(pwd)/node_modules/.bin:$PATH
  '';
}
