// SPIKE_NO_WIRE=1 builds the same app without the Rust crate, for the bundle
// size delta: the native module is dropped from autolinking and Metro swaps
// the JS package for spike/no-wire.ts.
const noWire = process.env.SPIKE_NO_WIRE === '1';
module.exports = {
  dependencies: noWire ? { 'react-native-ainb-wire': { platforms: { android: null, ios: null } } } : {},
};
