// Bundle-size baseline only: same exports as the uniffi binding, no native code.
export const wireVersion = () => 'no-wire baseline';
export const generateDeviceKeypair = () => ({ privateKey: new ArrayBuffer(32), publicKey: new ArrayBuffer(32) });
export async function connect(): Promise<never> {
  throw new Error('no-wire baseline build');
}
export type WireSessionLike = {
  close(): void;
  hello(token: string, deviceId: string, displayName: string): Promise<{ rttMs: number; selectedProtocol: number; capabilities: number }>;
  fleetSubscribe(after: bigint): Promise<{ rttMs: number; headRevision: bigint; replayState: string }>;
  mark(label: string): Promise<number>;
  stats(): { wsConnectMs: number; noiseHandshakeMs: number; heartbeatsSent: bigint; pongsReceived: bigint; lastPongRttMs: number; closed: boolean; closeReason: string };
};
