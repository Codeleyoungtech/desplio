import { useEffect, useRef } from 'react';

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
