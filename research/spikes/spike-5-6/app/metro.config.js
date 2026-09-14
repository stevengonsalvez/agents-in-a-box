const { getDefaultConfig } = require('expo/metro-config');
const path = require('path');

const config = getDefaultConfig(__dirname);
if (process.env.SPIKE_NO_WIRE === '1') {
  const stub = path.resolve(__dirname, 'spike/no-wire.ts');
  const upstream = config.resolver.resolveRequest;
  config.resolver.resolveRequest = (context, moduleName, platform) =>
    moduleName === 'react-native-ainb-wire'
      ? { type: 'sourceFile', filePath: stub }
      : (upstream ?? context.resolveRequest)(context, moduleName, platform);
}
module.exports = config;
