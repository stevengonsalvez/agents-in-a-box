import { Platform } from 'react-native';
import * as SecureStore from 'expo-secure-store';

import { generateDeviceKeypair } from 'react-native-ainb-wire';

export type KeyCustody = {
  privateKey: ArrayBuffer;
  created: boolean;
  loadMs: number;
  fingerprint: string;
  api: string;
};

const KEY = 'ainb_device_static_v1';
const GATED_KEY = 'ainb_device_static_gated_probe';

// Readable after first unlock so a backgrounded reconnect works on a locked
// phone; never migrates to another device through a backup.
const OPTIONS: SecureStore.SecureStoreOptions = {
  keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY,
};

const toB64 = (buf: ArrayBuffer) => btoa(String.fromCharCode(...new Uint8Array(buf)));
const fromB64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0)).buffer;
const hex = (buf: ArrayBuffer) => Array.from(new Uint8Array(buf).slice(0, 4), (b) => b.toString(16).padStart(2, '0')).join('');

export async function loadDeviceKey(): Promise<KeyCustody> {
  const t0 = Date.now();
  const api =
    Platform.OS === 'ios'
      ? 'iOS Keychain (kSecClassGenericPassword, AfterFirstUnlockThisDeviceOnly) via expo-secure-store'
      : 'Android Keystore AES-GCM key wrapping SharedPreferences ciphertext via expo-secure-store';
  const stored = await SecureStore.getItemAsync(KEY, OPTIONS);
  if (stored) {
    const { priv, pub } = JSON.parse(stored) as { priv: string; pub: string };
    return { privateKey: fromB64(priv), created: false, loadMs: Date.now() - t0, fingerprint: hex(fromB64(pub)), api };
  }
  const kp = generateDeviceKeypair();
  await SecureStore.setItemAsync(KEY, JSON.stringify({ priv: toB64(kp.privateKey), pub: toB64(kp.publicKey) }), OPTIONS);
  return { privateKey: kp.privateKey, created: true, loadMs: Date.now() - t0, fingerprint: hex(kp.publicKey), api };
}

/** Writes and reads a throwaway item behind `requireAuthentication`, then deletes it. */
export async function probeBiometricGate(): Promise<string> {
  const capable = SecureStore.canUseBiometricAuthentication();
  try {
    await SecureStore.setItemAsync(GATED_KEY, 'probe', { ...OPTIONS, requireAuthentication: true });
    const read = await SecureStore.getItemAsync(GATED_KEY, { ...OPTIONS, requireAuthentication: true });
    await SecureStore.deleteItemAsync(GATED_KEY, { requireAuthentication: true });
    return `capable=${capable} read=${read === 'probe'}`;
  } catch (e) {
    return `capable=${capable} error=${String(e).slice(0, 120)}`;
  }
}
