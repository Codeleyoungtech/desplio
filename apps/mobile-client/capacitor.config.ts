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
