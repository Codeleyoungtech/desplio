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
