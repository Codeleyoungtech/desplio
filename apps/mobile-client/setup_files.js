const fs = require('fs');
const path = require('path');

const write = (file, content) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, content.trim() + '\n');
};

write('capacitor.config.ts', `
import { CapacitorConfig } from '@capacitor/cli';

const config: CapacitorConfig = {
    appId: 'com.desplio.mobile',
    appName: 'Desplio',
    webDir: 'dist',
    server: {
        url: process.env.DEV ? 'http://localhost:5173' : undefined,
        cleartext: true,
    },
    android: {
        allowMixedContent: true,
        captureInput: true,
    },
    ios: {
        allowsLinkPreview: false,
        scrollEnabled: false,
    },
};
export default config;
`);

write('src/hooks/useWebRTC.ts', `
import { useRef, useState, useCallback } from 'react';

export function useWebRTC(onConnected: () => void) {
    const pcRef = useRef<RTCPeerConnection | null>(null);
    const dcRef = useRef<RTCDataChannel | null>(null);
    const [state, setState] = useState<'idle' | 'connecting' | 'connected' | 'error'>('idle');

    const connect = useCallback(async (hostIp: string, pin: string) => {
        setState('connecting');

        const pc = new RTCPeerConnection({
            iceServers: [{ urls: 'stun:stun.l.google.com:19302' }],
            iceTransportPolicy: 'all',
        });
        pcRef.current = pc;

        const dc = pc.createDataChannel('input', {
            ordered: true,
            maxPacketLifetime: 200,
        });
        dcRef.current = dc;

        pc.ontrack = ({ track, streams }) => {
            if (track.kind === 'video') {
                // handleVideoTrack(streams[0]);
            }
        };

        pc.onicecandidate = ({ candidate }) => {
            if (candidate) {
                // ws.send(JSON.stringify({ type: 'ice', ...candidate.toJSON() }));
            }
        };

        pc.onconnectionstatechange = () => {
            if (pc.connectionState === 'connected') {
                setState('connected');
                onConnected();
            } else if (['disconnected', 'failed', 'closed'].includes(pc.connectionState)) {
                setState('idle');
            }
        };

        const ws = new WebSocket(\`ws://\${hostIp}:7472/signal\`);
        ws.onopen = async () => ws.send(JSON.stringify({
            type: 'hello',
            pin,
            capabilities: {
                maxResolution: \`\${window.screen.width}x\${window.screen.height}\`,
                dpi: window.devicePixelRatio * 96,
                platform: 'android',
            }
        }));

        ws.onmessage = async ({ data }) => {
            const msg = JSON.parse(data);
            if (msg.type === 'offer') {
                await pc.setRemoteDescription({ type: 'offer', sdp: msg.sdp });
                const answer = await pc.createAnswer();
                await pc.setLocalDescription(answer);
                ws.send(JSON.stringify({ type: 'answer', sdp: answer.sdp }));
            } else if (msg.type === 'ice') {
                await pc.addIceCandidate(msg).catch(console.error);
            }
        };
    }, []);

    const sendInput = useCallback((event: any) => {
        if (dcRef.current?.readyState === 'open') {
            dcRef.current.send(JSON.stringify(event));
        }
    }, []);

    return { connect, sendInput, state };
}
`);

write('src/hooks/useInputCapture.ts', `
import { useEffect } from 'react';

interface InputEvent {
    t: 'touch' | 'key' | 'scroll' | 'ptr';
    ts: number;
    [key: string]: any;
}

export function useInputCapture(
    canvasRef: React.RefObject<HTMLElement>,
    displayBounds: { x: number; y: number; w: number; h: number },
    sendInput: (e: InputEvent) => void
) {
    useEffect(() => {
        const el = canvasRef.current;
        if (!el) return;

        const normalise = (clientX: number, clientY: number) => {
            const nx = (clientX - displayBounds.x) / displayBounds.w;
            const ny = (clientY - displayBounds.y) / displayBounds.h;
            return { nx: Math.max(0, Math.min(1, nx)), ny: Math.max(0, Math.min(1, ny)) };
        };

        const onPointerMove = (e: PointerEvent) => {
            const { nx, ny } = normalise(e.clientX, e.clientY);
            sendInput({ t: 'ptr', x: nx, y: ny, b: 0, ts: Date.now() });
        };

        const onPointerDown = (e: PointerEvent) => {
            el.setPointerCapture(e.pointerId);
            const { nx, ny } = normalise(e.clientX, e.clientY);
            sendInput({ t: 'ptr', x: nx, y: ny, b: e.button, down: true, ts: Date.now() });
        };

        const onPointerUp = (e: PointerEvent) => {
            const { nx, ny } = normalise(e.clientX, e.clientY);
            sendInput({ t: 'ptr', x: nx, y: ny, b: e.button, down: false, ts: Date.now() });
        };

        const onWheel = (e: WheelEvent) => {
            e.preventDefault();
            sendInput({ t: 'scroll', dx: Math.round(e.deltaX / 40), dy: Math.round(e.deltaY / 40), ts: Date.now() });
        };

        el.addEventListener('pointermove', onPointerMove);
        el.addEventListener('pointerdown', onPointerDown);
        el.addEventListener('pointerup', onPointerUp);
        el.addEventListener('wheel', onWheel, { passive: false });

        return () => {
            el.removeEventListener('pointermove', onPointerMove);
            el.removeEventListener('pointerdown', onPointerDown);
            el.removeEventListener('pointerup', onPointerUp);
            el.removeEventListener('wheel', onWheel);
        };
    }, [canvasRef, displayBounds, sendInput]);
}
`);

write('src/hooks/useWakeLock.ts', `
import { useEffect } from 'react';
import { KeepAwake } from '@capacitor-community/keep-awake';

export function useWakeLock(active: boolean) {
    useEffect(() => {
        if (active) {
            KeepAwake.keepAwake();
            return () => { KeepAwake.allowSleep(); };
        }
    }, [active]);
}
`);

write('src/screens/Display.tsx', `
import React, { useEffect, useRef } from 'react';

export function DisplayScreen({ stream }: { stream: MediaStream }) {
    const canvasRef = useRef<HTMLCanvasElement>(null);

    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas || !stream) return;

        const offscreen = canvas.transferControlToOffscreen();
        const worker = new Worker(new URL('../workers/renderer.worker.ts', import.meta.url));
        worker.postMessage({ canvas: offscreen, stream }, [offscreen]);

        return () => worker.terminate();
    }, [stream]);

    return (
        <canvas
            ref={canvasRef}
            style={{ width: '100vw', height: '100vh', touchAction: 'none', display: 'block' }}
        />
    );
}
`);

write('src/workers/renderer.worker.ts', `
self.onmessage = async ({ data: { canvas, stream } }) => {
    const ctx = canvas.getContext('2d');
    const [videoTrack] = stream.getVideoTracks();
    const reader = new (self as any).MediaStreamTrackProcessor({ track: videoTrack }).readable.getReader();

    while (true) {
        const { done, value: frame } = await reader.read();
        if (done) break;
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
    }
};
`);

console.log('Files created');
