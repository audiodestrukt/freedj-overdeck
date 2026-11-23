#!/bin/sh
# Build the reference mixer and install it for the local user (Linux):
# CLAP in ~/.clap, VST3 in ~/.vst3, so a DAW finds it too.
set -e
cd "$(dirname "$0")/.."
cargo build --release -p overdeck-mixer
so=target/release/liboverdeck_mixer.so
mkdir -p ~/.clap ~/.vst3/OverdeckMixer.vst3/Contents/x86_64-linux
cp "$so" ~/.clap/OverdeckMixer.clap
cp "$so" ~/.vst3/OverdeckMixer.vst3/Contents/x86_64-linux/OverdeckMixer.so
echo "installed ~/.clap/OverdeckMixer.clap and ~/.vst3/OverdeckMixer.vst3"
