# Source me: puts a rustup toolchain with the wasm32 target, trunk and wasm-bindgen on PATH (NixOS).
#   source scripts/dev-env.sh
export PATH="$(nix build nixpkgs#rustup --no-link --print-out-paths)/bin:$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN=1.95.0
# Debug info off: much smaller target/ (the workspace builds wgpu, codecs and egui).
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
