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
