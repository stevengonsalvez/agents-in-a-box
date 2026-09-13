import { useCallback, useEffect, useRef, useState } from 'react';
import { AppState, Platform, Pressable, ScrollView, StyleSheet, Text, View } from 'react-native';
import { StatusBar } from 'expo-status-bar';
import * as Linking from 'expo-linking';
import * as Notifications from 'expo-notifications';

import { connect, wireVersion, type WireSessionLike } from 'react-native-ainb-wire';
import { loadDeviceKey, probeBiometricGate, type KeyCustody } from './custody';
import pairing from './pairing.json';

type Row = { label: string; value: string; tone?: 'ok' | 'bad' | 'dim' };

const HEARTBEAT_MS = 15_000n;
const jsStartMs = (globalThis as { __jsStartMs?: number }).__jsStartMs ?? Date.now();

const b64ToBuf = (s: string): ArrayBuffer => Uint8Array.from(atob(s), (c) => c.charCodeAt(0)).buffer;

Notifications.setNotificationHandler({
  handleNotification: async () => ({ shouldShowBanner: true, shouldShowList: true, shouldPlaySound: false, shouldSetBadge: false }),
});

export default function App() {
  const session = useRef<WireSessionLike | null>(null);
  const [rows, setRows] = useState<Row[]>([]);
  const [log, setLog] = useState<string[]>([]);
  const [custody, setCustody] = useState<KeyCustody | null>(null);
  const [live, setLive] = useState<'connecting' | 'live' | 'failed' | 'closed'>('connecting');
  const [stats, setStats] = useState<ReturnType<WireSessionLike['stats']> | null>(null);

  const note = useCallback((line: string) => {
    const stamp = new Date().toISOString().slice(11, 23);
    setLog((l) => [`${stamp} ${line}`, ...l].slice(0, 40));
    session.current?.mark(line).catch(() => undefined);
  }, []);

  const schedule = useCallback(
    async (seconds: number) => {
      const perm = await Notifications.requestPermissionsAsync();
      const id = await Notifications.scheduleNotificationAsync({
        content: { title: 'needs input', body: `spike banner scheduled ${seconds}s ahead`, data: { seconds } },
        trigger: { type: Notifications.SchedulableTriggerInputTypes.TIME_INTERVAL, seconds, channelId: 'needs-input' },
      });
      note(`banner_scheduled seconds=${seconds} id=${id} perm=${perm.status}`);
    },
    [note],
  );

  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (Platform.OS === 'android') {
        await Notifications.setNotificationChannelAsync('needs-input', { name: 'Needs input', importance: Notifications.AndroidImportance.HIGH });
      }
      const key = await loadDeviceKey();
      if (cancelled) return;
      setCustody(key);
      const tKey = Date.now();
      try {
        const s = await connect({
          url: pairing.url,
          hostPublicKey: b64ToBuf(pairing.host_public),
          devicePrivateKey: key.privateKey,
          transport: pairing.transport,
          hostId: pairing.host_id,
          heartbeatIntervalMs: HEARTBEAT_MS,
        });
        session.current = s;
        const tConnected = Date.now();
        const hello = await s.hello(pairing.token, `spike-${Platform.OS}`, `spike ${Platform.OS}`);
        const tHello = Date.now();
        const sub = await s.fleetSubscribe(0n);
        const tSub = Date.now();
        const st = s.stats();
        setLive('live');
        setRows([
          { label: 'wire', value: wireVersion(), tone: 'dim' },
          { label: 'ws connect', value: `${st.wsConnectMs.toFixed(1)} ms` },
          { label: 'noise IK handshake', value: `${st.noiseHandshakeMs.toFixed(1)} ms` },
          { label: 'auth/hello', value: `${hello.rttMs.toFixed(1)} ms · proto ${hello.selectedProtocol} · ${hello.capabilities} caps`, tone: 'ok' },
          { label: 'fleet/subscribe', value: `${sub.rttMs.toFixed(1)} ms · head ${sub.headRevision} · ${sub.replayState}`, tone: 'ok' },
          { label: 'js start → first frame', value: `${tHello - jsStartMs} ms` },
          { label: 'key load', value: `${key.loadMs} ms · ${key.created ? 'minted' : 'reused'}` },
        ]);
        await s.mark(
          `cold_start js_start=${jsStartMs} key_ready=${tKey} connected=${tConnected} hello=${tHello} subscribed=${tSub} key_created=${key.created} key_fp=${key.fingerprint} ws_ms=${st.wsConnectMs.toFixed(1)} noise_ms=${st.noiseHandshakeMs.toFixed(1)}`,
        );
        // Warm reconnect in the same process: separates first-use cost (runtime,
        // library load, first socket) from the per-connection cost. No hello, so
        // the peer never mistakes it for the measured session.
        const tWarm = Date.now();
        const warm = await connect({
          url: pairing.url,
          hostPublicKey: b64ToBuf(pairing.host_public),
          devicePrivateKey: key.privateKey,
          transport: pairing.transport,
          hostId: pairing.host_id,
          heartbeatIntervalMs: 0n,
        });
        const ws = warm.stats();
        warm.close();
        await s.mark(`warm_connect total_ms=${Date.now() - tWarm} ws_ms=${ws.wsConnectMs.toFixed(1)} noise_ms=${ws.noiseHandshakeMs.toFixed(1)}`);
      } catch (e) {
        setLive('failed');
        setRows([{ label: 'error', value: String(e), tone: 'bad' }]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const sub = AppState.addEventListener('change', (state) => note(`appstate ${state}`));
    const link = Linking.addEventListener('url', ({ url }) => {
      const m = /schedule\/(\d+)/.exec(url);
      if (m) schedule(Number(m[1]));
    });
    const fired = Notifications.addNotificationReceivedListener((n) => note(`banner_received seconds=${n.request.content.data?.seconds}`));
    const tick = setInterval(() => {
      const s = session.current;
      if (!s) return;
      const st = s.stats();
      setStats(st);
      if (st.closed) setLive('closed');
    }, 1000);
    return () => {
      sub.remove();
      link.remove();
      fired.remove();
      clearInterval(tick);
    };
  }, [note, schedule]);

  return (
    <View style={styles.root}>
      <StatusBar style="light" />
      <ScrollView contentContainerStyle={styles.scroll}>
        <Text style={styles.kicker}>ainb · spike 5/6</Text>
        <Text style={styles.title}>Wire over Noise IK</Text>
        <View style={styles.pillRow}>
          <View style={[styles.pill, live === 'live' ? styles.pillOk : live === 'connecting' ? styles.pillDim : styles.pillBad]}>
            <Text style={styles.pillText}>{live}</Text>
          </View>
          {stats && (
            <Text style={styles.dim}>
              heartbeat 15 s · sent {String(stats.heartbeatsSent)} · pong {String(stats.pongsReceived)}
              {stats.lastPongRttMs ? ` · ${stats.lastPongRttMs.toFixed(1)} ms` : ''}
            </Text>
          )}
        </View>

        <View style={styles.card}>
          {rows.map((r) => (
            <View key={r.label} style={styles.row}>
              <Text style={styles.rowLabel}>{r.label}</Text>
              <Text style={[styles.rowValue, r.tone === 'ok' && styles.ok, r.tone === 'bad' && styles.bad, r.tone === 'dim' && styles.dim]}>{r.value}</Text>
            </View>
          ))}
        </View>

        {custody && (
          <View style={styles.card}>
            <Text style={styles.cardTitle}>Device static key</Text>
            <Text style={styles.body}>{custody.api}</Text>
            <Text style={styles.dim}>fingerprint {custody.fingerprint} · {custody.created ? 'minted this launch' : 'survived from a previous launch'}</Text>
          </View>
        )}

        <View style={styles.card}>
          <Text style={styles.cardTitle}>Needs-input banner</Text>
          <View style={styles.buttons}>
            {[60, 300, 1800].map((s) => (
              <Pressable key={s} style={styles.button} onPress={() => schedule(s)}>
                <Text style={styles.buttonText}>{s / 60} min</Text>
              </Pressable>
            ))}
            <Pressable style={[styles.button, styles.buttonGhost]} onPress={async () => note(`biometric_probe ${await probeBiometricGate()}`)}>
              <Text style={styles.buttonText}>biometric gate</Text>
            </Pressable>
          </View>
        </View>

        <View style={styles.card}>
          <Text style={styles.cardTitle}>Lifecycle</Text>
          {log.map((l) => (
            <Text key={l} style={styles.mono}>
              {l}
            </Text>
          ))}
        </View>
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#0B0F14' },
  scroll: { padding: 20, paddingTop: 64, gap: 14 },
  kicker: { color: '#6B7A8C', fontSize: 12, letterSpacing: 1.5, textTransform: 'uppercase' },
  title: { color: '#F2F5F8', fontSize: 28, fontWeight: '700' },
  pillRow: { flexDirection: 'row', alignItems: 'center', gap: 10, flexWrap: 'wrap' },
  pill: { paddingHorizontal: 10, paddingVertical: 4, borderRadius: 999 },
  pillOk: { backgroundColor: '#123D2A' },
  pillBad: { backgroundColor: '#4A1B1B' },
  pillDim: { backgroundColor: '#1C2430' },
  pillText: { color: '#E6EDF3', fontSize: 12, fontWeight: '600' },
  card: { backgroundColor: '#121821', borderRadius: 14, padding: 14, gap: 8, borderWidth: 1, borderColor: '#1E2733' },
  cardTitle: { color: '#E6EDF3', fontSize: 15, fontWeight: '600' },
  row: { flexDirection: 'row', justifyContent: 'space-between', gap: 12 },
  rowLabel: { color: '#8B98A9', fontSize: 13 },
  rowValue: { color: '#E6EDF3', fontSize: 13, flexShrink: 1, textAlign: 'right' },
  body: { color: '#C9D4DF', fontSize: 13 },
  ok: { color: '#5BD69B' },
  bad: { color: '#FF7B72' },
  dim: { color: '#6B7A8C', fontSize: 12 },
  buttons: { flexDirection: 'row', flexWrap: 'wrap', gap: 8 },
  button: { backgroundColor: '#2563EB', paddingHorizontal: 14, paddingVertical: 10, borderRadius: 10 },
  buttonGhost: { backgroundColor: '#1C2430' },
  buttonText: { color: '#FFFFFF', fontWeight: '600' },
  mono: { color: '#9FB0C2', fontSize: 11, fontFamily: Platform.select({ ios: 'Menlo', default: 'monospace' }) },
});
