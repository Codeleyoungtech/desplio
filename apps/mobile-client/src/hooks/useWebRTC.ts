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
            maxPacketLifeTime: 200,
        });
        dcRef.current = dc;

        pc.ontrack = ({ track }) => {
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

        const ws = new WebSocket(`ws://${hostIp}:7472/signal`);
        ws.onopen = async () => ws.send(JSON.stringify({
            type: 'hello',
            pin,
            capabilities: {
                maxResolution: `${window.screen.width}x${window.screen.height}`,
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
